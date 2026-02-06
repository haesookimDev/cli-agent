use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::orchestrator::Orchestrator;
use crate::types::{RunRecord, RunRequest, SessionSummary, TaskProfile};

const MIN_REFRESH_MS: u64 = 500;
const MAX_REFRESH_MS: u64 = 10_000;
const MIN_LIST_LIMIT: usize = 5;
const MAX_LIST_LIMIT: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    Sessions,
    Runs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InputMode {
    Normal,
    TaskInput,
    ConfirmDelete(Uuid),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TuiSettings {
    default_profile: TaskProfile,
    auto_refresh_ms: u64,
    session_limit: usize,
    run_limit: usize,
    follow_active_session: bool,
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            default_profile: TaskProfile::General,
            auto_refresh_ms: 1200,
            session_limit: 50,
            run_limit: 60,
            follow_active_session: true,
        }
    }
}

impl TuiSettings {
    fn sanitize(mut self) -> Self {
        self.auto_refresh_ms = self.auto_refresh_ms.clamp(MIN_REFRESH_MS, MAX_REFRESH_MS);
        self.session_limit = self.session_limit.clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
        self.run_limit = self.run_limit.clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
        self
    }
}

#[derive(Debug, Clone)]
struct TuiState {
    settings: TuiSettings,
    mode: InputMode,
    focus: FocusPane,
    sessions: Vec<SessionSummary>,
    runs: Vec<RunRecord>,
    selected_session: usize,
    selected_run: usize,
    active_session: Option<Uuid>,
    task_input: String,
    status_text: String,
    replay_preview: Vec<String>,
    show_help: bool,
    should_quit: bool,
}

impl TuiState {
    fn new(settings: TuiSettings) -> Self {
        Self {
            settings,
            mode: InputMode::Normal,
            focus: FocusPane::Sessions,
            sessions: Vec::new(),
            runs: Vec::new(),
            selected_session: 0,
            selected_run: 0,
            active_session: None,
            task_input: String::new(),
            status_text: "loading...".to_string(),
            replay_preview: Vec::new(),
            show_help: false,
            should_quit: false,
        }
    }

    fn selected_session(&self) -> Option<&SessionSummary> {
        self.sessions.get(self.selected_session)
    }

    fn selected_run(&self) -> Option<&RunRecord> {
        self.runs.get(self.selected_run)
    }

    fn active_or_selected_session_id(&self) -> Option<Uuid> {
        self.active_session
            .or_else(|| self.selected_session().map(|s| s.session_id))
    }

    fn clamp_selection(&mut self) {
        if self.sessions.is_empty() {
            self.selected_session = 0;
        } else {
            self.selected_session = self.selected_session.min(self.sessions.len() - 1);
        }

        if self.runs.is_empty() {
            self.selected_run = 0;
        } else {
            self.selected_run = self.selected_run.min(self.runs.len() - 1);
        }
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status_text = text.into();
    }
}

pub async fn run_tui(orchestrator: Orchestrator, settings_path: PathBuf) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = TuiState::new(load_settings(settings_path.as_path())?);
    let mut last_refresh = Instant::now() - Duration::from_secs(30);

    let result = async {
        loop {
            let refresh_interval = Duration::from_millis(state.settings.auto_refresh_ms);
            if last_refresh.elapsed() >= refresh_interval {
                if let Err(err) = refresh_state(&orchestrator, &mut state).await {
                    state.set_status(format!("refresh failed: {err}"));
                }
                last_refresh = Instant::now();
            }

            terminal.draw(|frame| draw_ui(frame, &state))?;

            let polled = tokio::task::block_in_place(|| event::poll(Duration::from_millis(100)))?;
            if polled {
                let ev = tokio::task::block_in_place(event::read)?;
                if let Event::Key(key) = ev {
                    let mut immediate_refresh = false;
                    handle_key(
                        key,
                        &orchestrator,
                        &mut state,
                        settings_path.as_path(),
                        &mut immediate_refresh,
                    )
                    .await?;

                    if immediate_refresh {
                        if let Err(err) = refresh_state(&orchestrator, &mut state).await {
                            state.set_status(format!("refresh failed: {err}"));
                        }
                        last_refresh = Instant::now();
                    }
                    if state.should_quit {
                        break;
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn refresh_state(orchestrator: &Orchestrator, state: &mut TuiState) -> anyhow::Result<()> {
    state.sessions = orchestrator
        .list_sessions(state.settings.session_limit)
        .await?;

    let run_scope_session = if state.settings.follow_active_session {
        state.active_or_selected_session_id()
    } else {
        None
    };

    state.runs = if let Some(session_id) = run_scope_session {
        orchestrator
            .list_session_runs(session_id, state.settings.run_limit)
            .await?
    } else {
        orchestrator
            .list_recent_runs(state.settings.run_limit)
            .await?
    };

    if let Some(active) = state.active_session {
        if !state.sessions.iter().any(|s| s.session_id == active) {
            state.active_session = None;
        }
    }

    state.clamp_selection();
    state.set_status(format!(
        "sessions={} runs={} refresh={}ms",
        state.sessions.len(),
        state.runs.len(),
        state.settings.auto_refresh_ms
    ));
    Ok(())
}

async fn handle_key(
    key: KeyEvent,
    orchestrator: &Orchestrator,
    state: &mut TuiState,
    settings_path: &Path,
    immediate_refresh: &mut bool,
) -> anyhow::Result<()> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.should_quit = true;
        return Ok(());
    }

    match &mut state.mode {
        InputMode::TaskInput => {
            match key.code {
                KeyCode::Esc => {
                    state.mode = InputMode::Normal;
                    state.task_input.clear();
                    state.set_status("task input cancelled");
                }
                KeyCode::Enter => {
                    let task = state.task_input.trim().to_string();
                    if task.is_empty() {
                        state.set_status("task cannot be empty");
                    } else {
                        let req = RunRequest {
                            task,
                            profile: state.settings.default_profile,
                            session_id: state.active_or_selected_session_id(),
                        };
                        match orchestrator.submit_run(req).await {
                            Ok(sub) => {
                                state.active_session = Some(sub.session_id);
                                state.task_input.clear();
                                state.mode = InputMode::Normal;
                                state.set_status(format!(
                                    "run queued: {} (session {})",
                                    short_uuid(sub.run_id),
                                    short_uuid(sub.session_id)
                                ));
                                *immediate_refresh = true;
                            }
                            Err(err) => {
                                state.set_status(format!("run submission failed: {err}"));
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    state.task_input.pop();
                }
                KeyCode::Char(ch) => {
                    state.task_input.push(ch);
                }
                _ => {}
            }
            return Ok(());
        }
        InputMode::ConfirmDelete(session_id) => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let target = *session_id;
                    match orchestrator.delete_session(target).await {
                        Ok(()) => {
                            if state.active_session == Some(target) {
                                state.active_session = None;
                            }
                            state.mode = InputMode::Normal;
                            state.replay_preview.clear();
                            state.set_status(format!("session deleted: {}", short_uuid(target)));
                            *immediate_refresh = true;
                        }
                        Err(err) => {
                            state.mode = InputMode::Normal;
                            state.set_status(format!("delete failed: {err}"));
                        }
                    }
                }
                _ => {
                    state.mode = InputMode::Normal;
                    state.set_status("delete cancelled");
                }
            }
            return Ok(());
        }
        InputMode::Normal => {}
    }

    match key.code {
        KeyCode::Char('q') => {
            state.should_quit = true;
        }
        KeyCode::Tab => {
            state.focus = match state.focus {
                FocusPane::Sessions => FocusPane::Runs,
                FocusPane::Runs => FocusPane::Sessions,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => match state.focus {
            FocusPane::Sessions => {
                state.selected_session = state.selected_session.saturating_sub(1);
            }
            FocusPane::Runs => {
                state.selected_run = state.selected_run.saturating_sub(1);
            }
        },
        KeyCode::Down | KeyCode::Char('j') => match state.focus {
            FocusPane::Sessions => {
                if !state.sessions.is_empty() {
                    state.selected_session =
                        (state.selected_session + 1).min(state.sessions.len().saturating_sub(1));
                }
            }
            FocusPane::Runs => {
                if !state.runs.is_empty() {
                    state.selected_run =
                        (state.selected_run + 1).min(state.runs.len().saturating_sub(1));
                }
            }
        },
        KeyCode::Enter => match state.focus {
            FocusPane::Sessions => {
                if let Some(session_id) = state.selected_session().map(|s| s.session_id) {
                    state.active_session = Some(session_id);
                    state.set_status(format!("active session set: {}", short_uuid(session_id)));
                    *immediate_refresh = true;
                }
            }
            FocusPane::Runs => {
                if let Some(session_id) = state.selected_run().map(|r| r.session_id) {
                    state.active_session = Some(session_id);
                    state.set_status(format!(
                        "active session from run: {}",
                        short_uuid(session_id)
                    ));
                    *immediate_refresh = true;
                }
            }
        },
        KeyCode::Char('?') => {
            state.show_help = !state.show_help;
        }
        KeyCode::Char('r') => {
            *immediate_refresh = true;
            state.set_status("manual refresh");
        }
        KeyCode::Char('i') | KeyCode::Char('a') => {
            state.mode = InputMode::TaskInput;
            state.set_status("task input mode: enter to submit, esc to cancel");
        }
        KeyCode::Char('n') => {
            let session_id = Uuid::new_v4();
            match orchestrator.create_session(session_id).await {
                Ok(()) => {
                    state.active_session = Some(session_id);
                    state.set_status(format!("new session created: {}", short_uuid(session_id)));
                    *immediate_refresh = true;
                }
                Err(err) => state.set_status(format!("create session failed: {err}")),
            }
        }
        KeyCode::Char('c') => {
            if let Some(session_id) = state.selected_session().map(|s| s.session_id) {
                state.active_session = Some(session_id);
                state.set_status(format!("continue session: {}", short_uuid(session_id)));
                *immediate_refresh = true;
            }
        }
        KeyCode::Char('x') => {
            state.active_session = None;
            state.set_status("active session cleared");
            *immediate_refresh = true;
        }
        KeyCode::Char('d') => {
            if let Some(session_id) = state.selected_session().map(|s| s.session_id) {
                state.mode = InputMode::ConfirmDelete(session_id);
                state.set_status(format!(
                    "delete session {} ? press y to confirm",
                    short_uuid(session_id)
                ));
            }
        }
        KeyCode::Char('m') => {
            if let Some(session_id) = state.active_or_selected_session_id() {
                match orchestrator.compact_session(session_id).await {
                    Ok(()) => {
                        state.set_status(format!("session compacted: {}", short_uuid(session_id)));
                        *immediate_refresh = true;
                    }
                    Err(err) => state.set_status(format!("compact failed: {err}")),
                }
            }
        }
        KeyCode::Char('v') => {
            if let Some(session_id) = state.active_or_selected_session_id() {
                match orchestrator.replay_session(session_id).await {
                    Ok(events) => {
                        let mut lines = events
                            .iter()
                            .rev()
                            .take(12)
                            .map(|e| {
                                format!(
                                    "{} {:?} {}",
                                    e.event.timestamp.to_rfc3339(),
                                    e.event.event_type,
                                    compact_json(e.event.payload.to_string().as_str(), 88)
                                )
                            })
                            .collect::<Vec<_>>();
                        lines.reverse();
                        state.replay_preview = lines;
                        state.set_status(format!(
                            "loaded replay preview for {}",
                            short_uuid(session_id)
                        ));
                    }
                    Err(err) => state.set_status(format!("replay failed: {err}")),
                }
            }
        }
        KeyCode::Char('1') => {
            update_profile(state, TaskProfile::Planning, settings_path)?;
        }
        KeyCode::Char('2') => {
            update_profile(state, TaskProfile::Extraction, settings_path)?;
        }
        KeyCode::Char('3') => {
            update_profile(state, TaskProfile::Coding, settings_path)?;
        }
        KeyCode::Char('4') => {
            update_profile(state, TaskProfile::General, settings_path)?;
        }
        KeyCode::Char('p') => {
            state.settings.default_profile = cycle_profile(state.settings.default_profile);
            save_settings(settings_path, &state.settings)?;
            state.set_status(format!("profile set: {}", state.settings.default_profile));
        }
        KeyCode::Char('[') => {
            state.settings.auto_refresh_ms = (state.settings.auto_refresh_ms.saturating_sub(250))
                .clamp(MIN_REFRESH_MS, MAX_REFRESH_MS);
            save_settings(settings_path, &state.settings)?;
            state.set_status(format!(
                "refresh interval: {}ms",
                state.settings.auto_refresh_ms
            ));
        }
        KeyCode::Char(']') => {
            state.settings.auto_refresh_ms =
                (state.settings.auto_refresh_ms + 250).clamp(MIN_REFRESH_MS, MAX_REFRESH_MS);
            save_settings(settings_path, &state.settings)?;
            state.set_status(format!(
                "refresh interval: {}ms",
                state.settings.auto_refresh_ms
            ));
        }
        KeyCode::Char('-') => {
            state.settings.run_limit = state
                .settings
                .run_limit
                .saturating_sub(5)
                .clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
            save_settings(settings_path, &state.settings)?;
            state.set_status(format!("run list limit: {}", state.settings.run_limit));
            *immediate_refresh = true;
        }
        KeyCode::Char('=') => {
            state.settings.run_limit =
                (state.settings.run_limit + 5).clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
            save_settings(settings_path, &state.settings)?;
            state.set_status(format!("run list limit: {}", state.settings.run_limit));
            *immediate_refresh = true;
        }
        KeyCode::Char('f') => {
            state.settings.follow_active_session = !state.settings.follow_active_session;
            save_settings(settings_path, &state.settings)?;
            state.set_status(format!(
                "follow active session: {}",
                state.settings.follow_active_session
            ));
            *immediate_refresh = true;
        }
        _ => {}
    }

    Ok(())
}

fn draw_ui(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(6),
        ])
        .split(frame.area());

    let header = header_lines(state);
    frame.render_widget(
        Paragraph::new(header).block(Block::default().borders(Borders::ALL).title("Agent TUI")),
        root[0],
    );

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(32),
            Constraint::Percentage(34),
            Constraint::Percentage(34),
        ])
        .split(root[1]);

    draw_sessions(frame, body[0], state);
    draw_runs(frame, body[1], state);
    draw_details(frame, body[2], state);

    let footer_text = footer_lines(state);
    frame.render_widget(
        Paragraph::new(footer_text).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Status/Controls"),
        ),
        root[2],
    );
}

fn draw_sessions(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, state: &TuiState) {
    let items = if state.sessions.is_empty() {
        vec![ListItem::new("<no sessions>")]
    } else {
        state
            .sessions
            .iter()
            .map(|s| {
                let active_marker = if Some(s.session_id) == state.active_session {
                    "*"
                } else {
                    " "
                };
                let last = s
                    .last_task
                    .as_deref()
                    .map(|t| compact_json(t, 34))
                    .unwrap_or_else(|| "-".to_string());
                ListItem::new(format!(
                    "{} {} runs={} last={}",
                    active_marker,
                    short_uuid(s.session_id),
                    s.run_count,
                    last
                ))
            })
            .collect::<Vec<_>>()
    };

    let title = if state.focus == FocusPane::Sessions {
        "Sessions [focus]"
    } else {
        "Sessions"
    };

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL).title(title));

    let mut list_state = ListState::default();
    if !state.sessions.is_empty() {
        list_state.select(Some(state.selected_session));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_runs(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, state: &TuiState) {
    let items = if state.runs.is_empty() {
        vec![ListItem::new("<no runs>")]
    } else {
        state
            .runs
            .iter()
            .map(|r| {
                ListItem::new(format!(
                    "{} [{}] {}",
                    short_uuid(r.run_id),
                    r.status,
                    compact_json(r.task.as_str(), 36)
                ))
            })
            .collect::<Vec<_>>()
    };

    let title = if state.focus == FocusPane::Runs {
        "Runs [focus]"
    } else {
        "Runs"
    };

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL).title(title));

    let mut list_state = ListState::default();
    if !state.runs.is_empty() {
        list_state.select(Some(state.selected_run));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_details(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, state: &TuiState) {
    let mut lines = Vec::<Line>::new();

    lines.push(Line::from(Span::styled(
        "Settings",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(format!(
        "profile={} refresh={}ms follow_active={}",
        state.settings.default_profile,
        state.settings.auto_refresh_ms,
        state.settings.follow_active_session
    )));
    lines.push(Line::from(format!(
        "session_limit={} run_limit={}",
        state.settings.session_limit, state.settings.run_limit
    )));

    if let Some(active) = state.active_session {
        lines.push(Line::from(format!("active_session={}", short_uuid(active))));
    } else {
        lines.push(Line::from("active_session=<none>"));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Selected Run",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));

    if let Some(run) = state.selected_run() {
        lines.push(Line::from(format!(
            "run={} session={} status={}",
            short_uuid(run.run_id),
            short_uuid(run.session_id),
            run.status
        )));
        lines.push(Line::from(format!("profile={}", run.profile)));
        lines.push(Line::from(format!(
            "task={}",
            compact_json(run.task.as_str(), 60)
        )));
        lines.push(Line::from(format!("outputs={}", run.outputs.len())));

        for output in run.outputs.iter().take(5) {
            lines.push(Line::from(format!(
                "- {} {} {}ms",
                output.node_id, output.succeeded, output.duration_ms
            )));
        }
        if run.outputs.len() > 5 {
            lines.push(Line::from(format!("... +{}", run.outputs.len() - 5)));
        }
    } else {
        lines.push(Line::from("<no run selected>"));
    }

    if !state.replay_preview.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Replay Preview",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        for line in &state.replay_preview {
            lines.push(Line::from(compact_json(line, 64)));
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Details")),
        area,
    );
}

fn header_lines(state: &TuiState) -> Vec<Line<'static>> {
    let mode = match &state.mode {
        InputMode::Normal => "normal".to_string(),
        InputMode::TaskInput => "task-input".to_string(),
        InputMode::ConfirmDelete(id) => format!("confirm-delete({})", short_uuid(*id)),
    };

    let focus = match state.focus {
        FocusPane::Sessions => "sessions",
        FocusPane::Runs => "runs",
    };

    vec![Line::from(format!(
        "mode={} focus={} profile={} active_session={}",
        mode,
        focus,
        state.settings.default_profile,
        state
            .active_session
            .map(short_uuid)
            .unwrap_or_else(|| "<none>".to_string())
    ))]
}

fn footer_lines(state: &TuiState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    match &state.mode {
        InputMode::TaskInput => {
            lines.push(Line::from(format!("task> {}", state.task_input)));
            lines.push(Line::from("enter=submit esc=cancel"));
        }
        InputMode::ConfirmDelete(id) => {
            lines.push(Line::from(format!(
                "delete session {} ? y=confirm any=cancel",
                short_uuid(*id)
            )));
        }
        InputMode::Normal => {
            lines.push(Line::from(state.status_text.clone()));
            lines.push(Line::from(
                "tab switch pane | i add task | enter set active | c continue | n new session | d delete | x clear active",
            ));
            lines.push(Line::from(
                "1/2/3/4 profile | p cycle profile | [/ ] refresh | -/= run-limit | f follow-active | v replay | m compact | r refresh | q quit",
            ));
            if state.show_help {
                lines.push(Line::from(
                    "help: select session then enter/c, tasks run into active session for continuation.",
                ));
            }
        }
    }

    lines
}

fn short_uuid(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

fn compact_json(input: &str, limit: usize) -> String {
    let one_line = input.replace('\n', " ");
    if one_line.chars().count() <= limit {
        return one_line;
    }
    let mut out = one_line.chars().take(limit).collect::<String>();
    out.push_str("...");
    out
}

fn load_settings(path: &Path) -> anyhow::Result<TuiSettings> {
    if !path.exists() {
        return Ok(TuiSettings::default());
    }

    let raw = fs::read_to_string(path)?;
    let parsed: TuiSettings = serde_json::from_str(raw.as_str())?;
    Ok(parsed.sanitize())
}

fn save_settings(path: &Path, settings: &TuiSettings) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(&settings.clone().sanitize())?;
    fs::write(path, payload)?;
    Ok(())
}

fn update_profile(
    state: &mut TuiState,
    profile: TaskProfile,
    settings_path: &Path,
) -> anyhow::Result<()> {
    state.settings.default_profile = profile;
    save_settings(settings_path, &state.settings)?;
    state.set_status(format!("profile set: {}", state.settings.default_profile));
    Ok(())
}

fn cycle_profile(current: TaskProfile) -> TaskProfile {
    match current {
        TaskProfile::Planning => TaskProfile::Extraction,
        TaskProfile::Extraction => TaskProfile::Coding,
        TaskProfile::Coding => TaskProfile::General,
        TaskProfile::General => TaskProfile::Planning,
    }
}
