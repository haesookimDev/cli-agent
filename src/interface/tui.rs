use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
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
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::orchestrator::Orchestrator;
use crate::router::ProviderKind;
use crate::types::{RunRecord, RunRequest, RunTrace, SessionSummary, TaskProfile};

const MIN_REFRESH_MS: u64 = 500;
const MAX_REFRESH_MS: u64 = 10_000;
const MIN_LIST_LIMIT: usize = 5;
const MAX_LIST_LIMIT: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    Sessions,
    Runs,
    Details,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InputMode {
    Normal,
    TaskInput,
    Filter(FocusPane),
    ConfirmDelete(Uuid),
    Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TuiSettings {
    default_profile: TaskProfile,
    auto_refresh_ms: u64,
    session_limit: usize,
    run_limit: usize,
    follow_active_session: bool,
    #[serde(default)]
    disabled_providers: Vec<String>,
    #[serde(default)]
    disabled_models: Vec<String>,
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            default_profile: TaskProfile::General,
            auto_refresh_ms: 1200,
            session_limit: 50,
            run_limit: 60,
            follow_active_session: true,
            disabled_providers: Vec::new(),
            disabled_models: Vec::new(),
        }
    }
}

impl TuiSettings {
    fn sanitize(mut self) -> Self {
        self.auto_refresh_ms = self.auto_refresh_ms.clamp(MIN_REFRESH_MS, MAX_REFRESH_MS);
        self.session_limit = self.session_limit.clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
        self.run_limit = self.run_limit.clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
        self.disabled_providers
            .retain(|p| parse_provider_kind(p).is_some());
        self.disabled_models.retain(|m| !m.is_empty());
        self
    }
}

#[derive(Debug, Clone)]
struct ModelInfo {
    model_id: String,
    display_name: String,
}

#[derive(Debug, Clone)]
struct TuiState {
    settings: TuiSettings,
    mode: InputMode,
    focus: FocusPane,
    sessions: Vec<SessionSummary>,
    filtered_sessions: Vec<usize>,
    runs: Vec<RunRecord>,
    filtered_runs: Vec<usize>,
    selected_session: usize,
    selected_run: usize,
    active_session: Option<Uuid>,
    session_filter: String,
    run_filter: String,
    task_input: String,
    status_text: String,
    replay_preview: Vec<String>,
    trace: Option<RunTrace>,
    details_scroll: u16,
    settings_selected: usize,
    model_list: Vec<ModelInfo>,
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
            filtered_sessions: Vec::new(),
            runs: Vec::new(),
            filtered_runs: Vec::new(),
            selected_session: 0,
            selected_run: 0,
            active_session: None,
            session_filter: String::new(),
            run_filter: String::new(),
            task_input: String::new(),
            status_text: "loading...".to_string(),
            replay_preview: Vec::new(),
            trace: None,
            details_scroll: 0,
            settings_selected: 0,
            model_list: Vec::new(),
            show_help: false,
            should_quit: false,
        }
    }

    fn selected_session(&self) -> Option<&SessionSummary> {
        self.selected_session_index()
            .and_then(|idx| self.sessions.get(idx))
    }

    fn selected_run(&self) -> Option<&RunRecord> {
        self.selected_run_index().and_then(|idx| self.runs.get(idx))
    }

    fn active_or_selected_session_id(&self) -> Option<Uuid> {
        self.active_session
            .or_else(|| self.selected_session().map(|s| s.session_id))
    }

    fn clamp_selection(&mut self) {
        if self.filtered_sessions.is_empty() {
            self.selected_session = 0;
        } else {
            self.selected_session = self
                .selected_session
                .min(self.filtered_sessions.len().saturating_sub(1));
        }

        if self.filtered_runs.is_empty() {
            self.selected_run = 0;
        } else {
            self.selected_run = self
                .selected_run
                .min(self.filtered_runs.len().saturating_sub(1));
        }
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.status_text = text.into();
    }

    fn selected_session_index(&self) -> Option<usize> {
        self.filtered_sessions.get(self.selected_session).copied()
    }

    fn selected_run_index(&self) -> Option<usize> {
        self.filtered_runs.get(self.selected_run).copied()
    }

    fn session_view_len(&self) -> usize {
        self.filtered_sessions.len()
    }

    fn run_view_len(&self) -> usize {
        self.filtered_runs.len()
    }

    fn rebuild_views(&mut self) {
        self.filtered_sessions = self
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(idx, session)| {
                if matches_session_filter(session, self.session_filter.as_str()) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        self.filtered_runs = self
            .runs
            .iter()
            .enumerate()
            .filter_map(|(idx, run)| {
                if matches_run_filter(run, self.run_filter.as_str()) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        self.clamp_selection();
    }

    fn active_filter_mut(&mut self, pane: FocusPane) -> &mut String {
        match pane {
            FocusPane::Sessions => &mut self.session_filter,
            FocusPane::Runs => &mut self.run_filter,
            FocusPane::Details => &mut self.session_filter, // unused; Details has no filter
        }
    }
}

pub async fn run_tui(orchestrator: Orchestrator, settings_path: PathBuf) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = TuiState::new(load_settings(settings_path.as_path())?);

    // Build model list from catalog
    state.model_list = orchestrator
        .router()
        .catalog()
        .iter()
        .map(|m| {
            let display_label = match m.provider {
                ProviderKind::OpenAi => "OpenAI",
                ProviderKind::Anthropic => "Anthropic",
                ProviderKind::Gemini => "Gemini",
                ProviderKind::Vllm => "vLLM",
                ProviderKind::Mock => "Mock",
            };
            ModelInfo {
                model_id: m.model_id.clone(),
                display_name: format!("{} ({})", display_label, m.model_id),
            }
        })
        .collect();

    // Restore disabled providers from saved settings
    for name in &state.settings.disabled_providers {
        if let Some(provider) = parse_provider_kind(name) {
            orchestrator.router().set_provider_disabled(provider, true);
        }
    }

    // Restore disabled models from saved settings
    for model_id in &state.settings.disabled_models {
        orchestrator.router().set_model_disabled(model_id, true);
    }

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

    state.rebuild_views();

    let old_trace_run_id = state.trace.as_ref().map(|t| t.run_id);
    state.trace = if let Some(run_id) = state.selected_run().map(|r| r.run_id) {
        orchestrator.get_run_trace(run_id, 2_000).await?
    } else {
        None
    };
    if state.trace.as_ref().map(|t| t.run_id) != old_trace_run_id {
        state.details_scroll = 0;
    }

    state.set_status(format!(
        "sessions={}/{} runs={}/{} refresh={}ms",
        state.filtered_sessions.len(),
        state.sessions.len(),
        state.filtered_runs.len(),
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

    match state.mode.clone() {
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
                            workflow_id: None,
                            workflow_params: None,
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
        InputMode::Filter(target) => {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    state.mode = InputMode::Normal;
                    state.set_status("filter mode closed");
                }
                KeyCode::Backspace => {
                    {
                        let filter = state.active_filter_mut(target);
                        filter.pop();
                    }
                    state.rebuild_views();
                    let current = match &target {
                        FocusPane::Sessions => state.session_filter.as_str(),
                        FocusPane::Runs => state.run_filter.as_str(),
                        FocusPane::Details => "",
                    };
                    state.set_status(format!("{} filter: '{}'", pane_label(target), current));
                }
                KeyCode::Char(ch) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        return Ok(());
                    }
                    {
                        let filter = state.active_filter_mut(target);
                        filter.push(ch);
                    }
                    state.rebuild_views();
                    let current = match &target {
                        FocusPane::Sessions => state.session_filter.as_str(),
                        FocusPane::Runs => state.run_filter.as_str(),
                        FocusPane::Details => "",
                    };
                    state.set_status(format!("{} filter: '{}'", pane_label(target), current));
                }
                _ => {}
            }
            return Ok(());
        }
        InputMode::ConfirmDelete(session_id) => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let target = session_id;
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
        InputMode::Settings => {
            let model_count = state.model_list.len();
            // 0..4 = settings, 5 = separator, 6..6+model_count-1 = models
            let separator_idx = 5;
            let settings_max = separator_idx + model_count; // last selectable index

            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    state.mode = InputMode::Normal;
                    state.set_status("settings closed");
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let prev = state.settings_selected.saturating_sub(1);
                    // skip separator
                    let prev = if prev == separator_idx { separator_idx - 1 } else { prev };
                    state.settings_selected = prev;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let next = state.settings_selected + 1;
                    // skip separator
                    let next = if next == separator_idx { separator_idx + 1 } else { next };
                    state.settings_selected = next.min(settings_max);
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    match state.settings_selected {
                        0 => {
                            state.settings.default_profile =
                                cycle_profile_back(state.settings.default_profile);
                        }
                        1 => {
                            state.settings.auto_refresh_ms =
                                state.settings.auto_refresh_ms.saturating_sub(250)
                                    .clamp(MIN_REFRESH_MS, MAX_REFRESH_MS);
                        }
                        2 => {
                            state.settings.session_limit =
                                state.settings.session_limit.saturating_sub(5)
                                    .clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
                        }
                        3 => {
                            state.settings.run_limit =
                                state.settings.run_limit.saturating_sub(5)
                                    .clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
                        }
                        4 => {
                            state.settings.follow_active_session =
                                !state.settings.follow_active_session;
                        }
                        idx if idx > separator_idx => {
                            toggle_model(state, orchestrator, idx - separator_idx - 1);
                        }
                        _ => {}
                    }
                    save_settings(settings_path, &state.settings)?;
                    *immediate_refresh = true;
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                    match state.settings_selected {
                        0 => {
                            state.settings.default_profile =
                                cycle_profile(state.settings.default_profile);
                        }
                        1 => {
                            state.settings.auto_refresh_ms =
                                (state.settings.auto_refresh_ms + 250)
                                    .clamp(MIN_REFRESH_MS, MAX_REFRESH_MS);
                        }
                        2 => {
                            state.settings.session_limit =
                                (state.settings.session_limit + 5)
                                    .clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
                        }
                        3 => {
                            state.settings.run_limit =
                                (state.settings.run_limit + 5)
                                    .clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
                        }
                        4 => {
                            state.settings.follow_active_session =
                                !state.settings.follow_active_session;
                        }
                        idx if idx > separator_idx => {
                            toggle_model(state, orchestrator, idx - separator_idx - 1);
                        }
                        _ => {}
                    }
                    save_settings(settings_path, &state.settings)?;
                    *immediate_refresh = true;
                }
                KeyCode::Char('R') => {
                    // Re-enable all previously disabled providers
                    for name in &state.settings.disabled_providers {
                        if let Some(provider) = parse_provider_kind(name) {
                            orchestrator.router().set_provider_disabled(provider, false);
                        }
                    }
                    // Re-enable all previously disabled models
                    for model_id in &state.settings.disabled_models {
                        orchestrator.router().set_model_disabled(model_id, false);
                    }
                    state.settings = TuiSettings::default();
                    save_settings(settings_path, &state.settings)?;
                    state.set_status("settings reset to defaults");
                    *immediate_refresh = true;
                }
                _ => {}
            }
            return Ok(());
        }
        InputMode::Normal => {}
    }

    match key.code {
        KeyCode::Char('q') => {
            state.should_quit = true;
        }
        KeyCode::Char('/') => {
            if state.focus == FocusPane::Details {
                state.set_status("details pane does not support filtering");
            } else {
                state.mode = InputMode::Filter(state.focus);
                let current = match state.focus {
                    FocusPane::Sessions => state.session_filter.as_str(),
                    FocusPane::Runs => state.run_filter.as_str(),
                    FocusPane::Details => "",
                };
                state.set_status(format!(
                    "{} filter mode: '{}'",
                    pane_label(state.focus),
                    current
                ));
            }
        }
        KeyCode::Char('0') => {
            if state.focus == FocusPane::Details {
                state.set_status("details pane does not support filtering");
            } else {
                let cleared = {
                    let filter = state.active_filter_mut(state.focus);
                    if filter.is_empty() {
                        false
                    } else {
                        filter.clear();
                        true
                    }
                };
                if cleared {
                    state.rebuild_views();
                    state.set_status(format!("{} filter cleared", pane_label(state.focus)));
                } else {
                    state.set_status(format!(
                        "{} filter already empty",
                        pane_label(state.focus)
                    ));
                }
            }
        }
        KeyCode::Tab => {
            state.focus = match state.focus {
                FocusPane::Sessions => FocusPane::Runs,
                FocusPane::Runs => FocusPane::Details,
                FocusPane::Details => FocusPane::Sessions,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => match state.focus {
            FocusPane::Sessions => {
                state.selected_session = state.selected_session.saturating_sub(1);
            }
            FocusPane::Runs => {
                state.selected_run = state.selected_run.saturating_sub(1);
            }
            FocusPane::Details => {
                state.details_scroll = state.details_scroll.saturating_sub(1);
            }
        },
        KeyCode::Down | KeyCode::Char('j') => match state.focus {
            FocusPane::Sessions => {
                if state.session_view_len() > 0 {
                    state.selected_session = (state.selected_session + 1)
                        .min(state.session_view_len().saturating_sub(1));
                }
            }
            FocusPane::Runs => {
                if state.run_view_len() > 0 {
                    state.selected_run =
                        (state.selected_run + 1).min(state.run_view_len().saturating_sub(1));
                }
            }
            FocusPane::Details => {
                state.details_scroll = state.details_scroll.saturating_add(1);
            }
        },
        KeyCode::PageUp => match state.focus {
            FocusPane::Sessions => {
                state.selected_session = state.selected_session.saturating_sub(8);
            }
            FocusPane::Runs => {
                state.selected_run = state.selected_run.saturating_sub(8);
            }
            FocusPane::Details => {
                state.details_scroll = state.details_scroll.saturating_sub(8);
            }
        },
        KeyCode::PageDown => match state.focus {
            FocusPane::Sessions => {
                if state.session_view_len() > 0 {
                    state.selected_session = (state.selected_session + 8)
                        .min(state.session_view_len().saturating_sub(1));
                }
            }
            FocusPane::Runs => {
                if state.run_view_len() > 0 {
                    state.selected_run =
                        (state.selected_run + 8).min(state.run_view_len().saturating_sub(1));
                }
            }
            FocusPane::Details => {
                state.details_scroll = state.details_scroll.saturating_add(8);
            }
        },
        KeyCode::Home => match state.focus {
            FocusPane::Sessions => state.selected_session = 0,
            FocusPane::Runs => state.selected_run = 0,
            FocusPane::Details => state.details_scroll = 0,
        },
        KeyCode::End => match state.focus {
            FocusPane::Sessions => {
                state.selected_session = state.session_view_len().saturating_sub(1);
            }
            FocusPane::Runs => {
                state.selected_run = state.run_view_len().saturating_sub(1);
            }
            FocusPane::Details => {
                state.details_scroll = u16::MAX / 2;
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
            FocusPane::Details => {
                state.details_scroll = 0;
            }
        },
        KeyCode::Char('?') => {
            state.show_help = !state.show_help;
        }
        KeyCode::Char('S') => {
            state.mode = InputMode::Settings;
            state.settings_selected = 0;
            state.set_status("settings opened");
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
        KeyCode::Char('s') => {
            if let Some(run_id) = state.selected_run().map(|r| r.run_id) {
                match orchestrator.cancel_run(run_id).await {
                    Ok(true) => {
                        state.set_status(format!("run cancelling: {}", short_uuid(run_id)));
                        *immediate_refresh = true;
                    }
                    Ok(false) => state.set_status("run cancel ignored"),
                    Err(err) => state.set_status(format!("cancel failed: {err}")),
                }
            }
        }
        KeyCode::Char('z') => {
            if let Some(run_id) = state.selected_run().map(|r| r.run_id) {
                match orchestrator.pause_run(run_id).await {
                    Ok(true) => {
                        state.set_status(format!("run paused: {}", short_uuid(run_id)));
                        *immediate_refresh = true;
                    }
                    Ok(false) => state.set_status("run pause ignored"),
                    Err(err) => state.set_status(format!("pause failed: {err}")),
                }
            }
        }
        KeyCode::Char('u') => {
            if let Some(run_id) = state.selected_run().map(|r| r.run_id) {
                match orchestrator.resume_run(run_id).await {
                    Ok(true) => {
                        state.set_status(format!("run resumed: {}", short_uuid(run_id)));
                        *immediate_refresh = true;
                    }
                    Ok(false) => state.set_status("run resume ignored"),
                    Err(err) => state.set_status(format!("resume failed: {err}")),
                }
            }
        }
        KeyCode::Char('t') => {
            if let Some(run_id) = state.selected_run().map(|r| r.run_id) {
                match orchestrator.retry_run(run_id).await {
                    Ok(sub) => {
                        state.active_session = Some(sub.session_id);
                        state.set_status(format!(
                            "retry queued: {} -> {}",
                            short_uuid(run_id),
                            short_uuid(sub.run_id)
                        ));
                        *immediate_refresh = true;
                    }
                    Err(err) => state.set_status(format!("retry failed: {err}")),
                }
            }
        }
        KeyCode::Char('g') => {
            if let Some(run_id) = state.selected_run().map(|r| r.run_id) {
                let target_session = state.active_or_selected_session_id();
                match orchestrator.clone_run(run_id, target_session).await {
                    Ok(sub) => {
                        state.active_session = Some(sub.session_id);
                        state.set_status(format!(
                            "clone queued: {} -> {}",
                            short_uuid(run_id),
                            short_uuid(sub.run_id)
                        ));
                        *immediate_refresh = true;
                    }
                    Err(err) => state.set_status(format!("clone failed: {err}")),
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
        KeyCode::Char(',') => {
            state.settings.session_limit = state
                .settings
                .session_limit
                .saturating_sub(5)
                .clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
            save_settings(settings_path, &state.settings)?;
            state.set_status(format!(
                "session list limit: {}",
                state.settings.session_limit
            ));
            *immediate_refresh = true;
        }
        KeyCode::Char('.') => {
            state.settings.session_limit =
                (state.settings.session_limit + 5).clamp(MIN_LIST_LIMIT, MAX_LIST_LIMIT);
            save_settings(settings_path, &state.settings)?;
            state.set_status(format!(
                "session list limit: {}",
                state.settings.session_limit
            ));
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
            Constraint::Length(4),
            Constraint::Min(12),
            Constraint::Length(4),
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
    let (footer_title, footer_border_color) = match &state.mode {
        InputMode::Normal => ("Controls", Color::DarkGray),
        InputMode::TaskInput => ("Task Input", Color::Blue),
        InputMode::Filter(_) => ("Filter", Color::Yellow),
        InputMode::ConfirmDelete(_) => ("Confirm Delete", Color::Red),
        InputMode::Settings => ("Settings", Color::Magenta),
    };
    frame.render_widget(
        Paragraph::new(footer_text).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(
                    format!(" {} ", footer_title),
                    Style::default()
                        .fg(footer_border_color)
                        .add_modifier(Modifier::BOLD),
                ))
                .border_style(Style::default().fg(footer_border_color)),
        ),
        root[2],
    );

    if state.show_help {
        draw_help_overlay(frame);
    }

    if matches!(state.mode, InputMode::Settings) {
        draw_settings_overlay(frame, state);
    }
}

fn draw_sessions(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, state: &TuiState) {
    let items = if state.filtered_sessions.is_empty() {
        if state.sessions.is_empty() {
            vec![ListItem::new(Span::styled(
                "  No sessions yet. Press 'n' to create one.",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            vec![ListItem::new(Span::styled(
                "  No sessions match filter. Press '0' to clear.",
                Style::default().fg(Color::DarkGray),
            ))]
        }
    } else {
        state
            .filtered_sessions
            .iter()
            .filter_map(|idx| state.sessions.get(*idx))
            .map(|s| {
                let active_marker = if Some(s.session_id) == state.active_session {
                    "*"
                } else {
                    " "
                };
                let last = s
                    .last_task
                    .as_deref()
                    .map(|t| compact_json(t, 24))
                    .unwrap_or_else(|| "-".to_string());
                let time_str = s
                    .last_run_at
                    .map(relative_time)
                    .unwrap_or_else(|| "new".to_string());
                let line = Line::from(vec![
                    Span::styled(
                        active_marker.to_string(),
                        if active_marker == "*" {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                    Span::styled(
                        short_uuid(s.session_id),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(format!("{}", s.run_count), Style::default().fg(Color::Cyan)),
                    Span::styled("r ", Style::default().fg(Color::DarkGray)),
                    Span::styled(time_str, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
                    Span::styled(last, Style::default().fg(Color::White)),
                ]);
                ListItem::new(line)
            })
            .collect::<Vec<_>>()
    };

    let filter_display = if state.session_filter.is_empty() {
        String::new()
    } else {
        format!(" [/{}]", compact_json(state.session_filter.as_str(), 14))
    };
    let title = format!(
        " Sessions {}/{}{}",
        state.filtered_sessions.len(),
        state.sessions.len(),
        filter_display,
    );

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(pane_block(title, FocusPane::Sessions, state.focus));

    let mut list_state = ListState::default();
    if !state.filtered_sessions.is_empty() {
        list_state.select(Some(state.selected_session));
    }
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn draw_runs(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, state: &TuiState) {
    let items = if state.filtered_runs.is_empty() {
        if state.runs.is_empty() {
            vec![ListItem::new(Span::styled(
                "  No runs yet. Press 'i' to submit a task.",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            vec![ListItem::new(Span::styled(
                "  No runs match filter. Press '0' to clear.",
                Style::default().fg(Color::DarkGray),
            ))]
        }
    } else {
        state
            .filtered_runs
            .iter()
            .filter_map(|idx| state.runs.get(*idx))
            .map(|r| {
                let status = r.status.to_string();
                let duration_text = if r.status.is_terminal() {
                    match (r.started_at, r.finished_at) {
                        (Some(start), Some(end)) => {
                            let ms = end
                                .signed_duration_since(start)
                                .num_milliseconds()
                                .max(0) as u128;
                            format!(" {}", format_duration_ms(ms))
                        }
                        _ => String::new(),
                    }
                } else {
                    String::new()
                };
                let line = Line::from(vec![
                    Span::styled(
                        short_uuid(r.run_id),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(format!("[{}]", status), run_status_style(status.as_str())),
                    Span::styled(duration_text, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
                    Span::raw(compact_json(r.task.as_str(), 28)),
                ]);
                ListItem::new(line)
            })
            .collect::<Vec<_>>()
    };

    let filter_display = if state.run_filter.is_empty() {
        String::new()
    } else {
        format!(" [/{}]", compact_json(state.run_filter.as_str(), 14))
    };
    let title = format!(
        " Runs {}/{}{}",
        state.filtered_runs.len(),
        state.runs.len(),
        filter_display,
    );

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
        .block(pane_block(title, FocusPane::Runs, state.focus));

    let mut list_state = ListState::default();
    if !state.filtered_runs.is_empty() {
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
        let status_text = run.status.to_string();
        lines.push(Line::from(vec![
            Span::styled("run=", Style::default().fg(Color::DarkGray)),
            Span::styled(
                short_uuid(run.run_id),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("session=", Style::default().fg(Color::DarkGray)),
            Span::styled(short_uuid(run.session_id), Style::default()),
            Span::raw("  "),
            Span::styled(
                format!("[{}]", status_text),
                run_status_style(status_text.as_str()),
            ),
        ]));
        lines.push(Line::from(format!("profile={}", run.profile)));
        lines.push(Line::from(format!(
            "task={}",
            compact_json(run.task.as_str(), 60)
        )));
        lines.push(Line::from(format!("outputs={}", run.outputs.len())));

        for output in run.outputs.iter().take(5) {
            let ok_marker = if output.succeeded { "ok" } else { "err" };
            let ok_style = if output.succeeded {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red)
            };
            lines.push(Line::from(vec![
                Span::styled("  - ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    output.node_id.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(ok_marker.to_string(), ok_style),
                Span::styled(
                    format!(" {}ms", output.duration_ms),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        if run.outputs.len() > 5 {
            lines.push(Line::from(Span::styled(
                format!("  ... +{}", run.outputs.len() - 5),
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "<no run selected>",
            Style::default().fg(Color::DarkGray),
        )));
    }

    if let Some(trace) = &state.trace {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Behavior Graph",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(format!(
            "nodes={} active={} completed={} failed={}",
            trace.graph.nodes.len(),
            trace.graph.active_nodes.len(),
            trace.graph.completed_nodes,
            trace.graph.failed_nodes
        )));

        for node in trace.graph.nodes.iter().take(8) {
            let deps = if node.dependencies.is_empty() {
                "-".to_string()
            } else {
                compact_json(node.dependencies.join(",").as_str(), 24)
            };
            let marker = status_marker(node.status.as_str());
            lines.push(Line::from(vec![
                Span::styled(
                    marker.to_string(),
                    run_status_style(node.status.as_str()),
                ),
                Span::raw(" "),
                Span::styled(
                    node.node_id.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" deps={}", deps),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!(
                        " model={}",
                        node.model.clone().unwrap_or_else(|| "-".to_string())
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        if trace.graph.nodes.len() > 8 {
            lines.push(Line::from(Span::styled(
                format!("... +{}", trace.graph.nodes.len() - 8),
                Style::default().fg(Color::DarkGray),
            )));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Execution Timeline",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        let timeline_width = area.width.saturating_sub(26).clamp(12, 36) as usize;
        lines.extend(build_timeline_lines(trace, timeline_width, 6));

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Action Mix",
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        )));
        for (action, count) in build_action_mix(trace, 4) {
            lines.push(Line::from(format!("{action}: {count}")));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Recent Actions",
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )));
        for event in trace.events.iter().rev().take(6).rev() {
            lines.push(Line::from(format!(
                "#{} {} {}",
                event.seq,
                event.action,
                compact_json(event.payload.to_string().as_str(), 46)
            )));
        }
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

    let scroll_indicator = if state.details_scroll > 0 {
        format!(" [+{}]", state.details_scroll)
    } else {
        String::new()
    };
    let title = format!(" Details{}", scroll_indicator);

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .scroll((state.details_scroll, 0))
            .block(pane_block(title, FocusPane::Details, state.focus)),
        area,
    );
}

fn header_lines(state: &TuiState) -> Vec<Line<'static>> {
    let (mode_text, mode_color) = match &state.mode {
        InputMode::Normal => (" NORMAL ", Color::Green),
        InputMode::TaskInput => (" INPUT ", Color::Blue),
        InputMode::Filter(_) => (" FILTER ", Color::Yellow),
        InputMode::ConfirmDelete(_) => (" DELETE? ", Color::Red),
        InputMode::Settings => (" SETTINGS ", Color::Magenta),
    };
    let mode_span = Span::styled(
        mode_text.to_string(),
        Style::default()
            .fg(Color::Black)
            .bg(mode_color)
            .add_modifier(Modifier::BOLD),
    );

    let focus_text = match state.focus {
        FocusPane::Sessions => "Sessions",
        FocusPane::Runs => "Runs",
        FocusPane::Details => "Details",
    };
    let focus_span = Span::styled(
        focus_text.to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let profile_color = match state.settings.default_profile {
        TaskProfile::Planning => Color::Magenta,
        TaskProfile::Extraction => Color::Cyan,
        TaskProfile::Coding => Color::Green,
        TaskProfile::General => Color::White,
    };
    let profile_span = Span::styled(
        format!("{}", state.settings.default_profile),
        Style::default()
            .fg(profile_color)
            .add_modifier(Modifier::BOLD),
    );

    let session_span = match state.active_session {
        Some(id) => Span::styled(
            short_uuid(id),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        None => Span::styled("none".to_string(), Style::default().fg(Color::DarkGray)),
    };

    let sep = Span::styled(" | ", Style::default().fg(Color::DarkGray));

    let line1 = Line::from(vec![
        Span::raw(" "),
        mode_span,
        sep.clone(),
        Span::styled("Focus: ", Style::default().fg(Color::DarkGray)),
        focus_span,
        sep.clone(),
        Span::styled("Profile: ", Style::default().fg(Color::DarkGray)),
        profile_span,
        sep.clone(),
        Span::styled("Session: ", Style::default().fg(Color::DarkGray)),
        session_span,
    ]);

    let line2 = Line::from(vec![
        Span::raw(" "),
        Span::styled(state.status_text.clone(), Style::default().fg(Color::White)),
    ]);

    vec![line1, line2]
}

fn footer_lines(state: &TuiState) -> Vec<Line<'static>> {
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::DarkGray);
    let input_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    match &state.mode {
        InputMode::TaskInput => {
            vec![
                Line::from(vec![
                    Span::styled(" task> ", key_style),
                    Span::styled(state.task_input.clone(), input_style),
                    Span::styled("_", Style::default().fg(Color::Yellow)),
                ]),
                Line::from(vec![
                    Span::styled(" Enter", key_style),
                    Span::styled(" submit  ", desc_style),
                    Span::styled("Esc", key_style),
                    Span::styled(" cancel", desc_style),
                ]),
            ]
        }
        InputMode::Filter(target) => {
            let current = match target {
                FocusPane::Sessions => state.session_filter.as_str(),
                FocusPane::Runs => state.run_filter.as_str(),
                FocusPane::Details => "",
            };
            vec![
                Line::from(vec![
                    Span::styled(format!(" {} filter> ", pane_label(*target)), key_style),
                    Span::styled(current.to_string(), input_style),
                    Span::styled("_", Style::default().fg(Color::Yellow)),
                ]),
                Line::from(vec![
                    Span::styled(" Enter/Esc", key_style),
                    Span::styled(" close  ", desc_style),
                    Span::styled("0", key_style),
                    Span::styled(" clear filter", desc_style),
                ]),
            ]
        }
        InputMode::ConfirmDelete(id) => {
            vec![Line::from(vec![
                Span::styled(
                    format!(" Delete session {}?  ", short_uuid(*id)),
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("y", key_style),
                Span::styled(" confirm  ", desc_style),
                Span::styled("any key", key_style),
                Span::styled(" cancel", desc_style),
            ])]
        }
        InputMode::Settings => {
            vec![Line::from(vec![
                Span::styled(" j/k", key_style),
                Span::styled(" navigate  ", desc_style),
                Span::styled("h/l", key_style),
                Span::styled(" change  ", desc_style),
                Span::styled("R", key_style),
                Span::styled(" reset  ", desc_style),
                Span::styled("Esc", key_style),
                Span::styled(" close", desc_style),
            ])]
        }
        InputMode::Normal => {
            let nav_line = Line::from(vec![
                Span::styled(" Tab", key_style),
                Span::styled(" pane  ", desc_style),
                Span::styled("j/k", key_style),
                Span::styled(" move  ", desc_style),
                Span::styled("/", key_style),
                Span::styled(" filter  ", desc_style),
                Span::styled("i", key_style),
                Span::styled(" task  ", desc_style),
                Span::styled("?", key_style),
                Span::styled(" help  ", desc_style),
                Span::styled("q", key_style),
                Span::styled(" quit", desc_style),
            ]);

            let context_line = match state.focus {
                FocusPane::Sessions => Line::from(vec![
                    Span::styled(" Enter", key_style),
                    Span::styled(" activate  ", desc_style),
                    Span::styled("n", key_style),
                    Span::styled(" new  ", desc_style),
                    Span::styled("c", key_style),
                    Span::styled(" continue  ", desc_style),
                    Span::styled("d", key_style),
                    Span::styled(" delete  ", desc_style),
                    Span::styled("x", key_style),
                    Span::styled(" clear  ", desc_style),
                    Span::styled("p", key_style),
                    Span::styled(" profile", desc_style),
                ]),
                FocusPane::Runs => Line::from(vec![
                    Span::styled(" s", key_style),
                    Span::styled(" cancel  ", desc_style),
                    Span::styled("z", key_style),
                    Span::styled(" pause  ", desc_style),
                    Span::styled("u", key_style),
                    Span::styled(" resume  ", desc_style),
                    Span::styled("t", key_style),
                    Span::styled(" retry  ", desc_style),
                    Span::styled("g", key_style),
                    Span::styled(" clone  ", desc_style),
                    Span::styled("r", key_style),
                    Span::styled(" refresh", desc_style),
                ]),
                FocusPane::Details => Line::from(vec![
                    Span::styled(" j/k", key_style),
                    Span::styled(" scroll  ", desc_style),
                    Span::styled("r", key_style),
                    Span::styled(" refresh  ", desc_style),
                    Span::styled("v", key_style),
                    Span::styled(" replay  ", desc_style),
                    Span::styled("Home", key_style),
                    Span::styled(" top  ", desc_style),
                    Span::styled("End", key_style),
                    Span::styled(" bottom", desc_style),
                ]),
            };

            vec![nav_line, context_line]
        }
    }
}

fn draw_help_overlay(frame: &mut ratatui::Frame<'_>) {
    let popup = centered_rect(74, 72, frame.area());
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            "Quick Help",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Navigation:"),
        Line::from("- tab switch pane, j/k or up/down move"),
        Line::from("- pgup/pgdn jump, home/end to first/last"),
        Line::from(""),
        Line::from("Filtering:"),
        Line::from("- / start filter on focused pane"),
        Line::from("- enter/esc close filter, 0 clear current filter"),
        Line::from(""),
        Line::from("Session / Run Actions:"),
        Line::from("- i add task, enter/c continue session, n new session"),
        Line::from("- s cancel, z pause, u resume, t retry, g clone"),
        Line::from("- d delete session, m compact, v replay preview"),
        Line::from(""),
        Line::from("Settings:"),
        Line::from("- S open settings panel"),
        Line::from("- 1/2/3/4 profile, p cycle"),
        Line::from("- [ ] refresh, , . session-limit, - = run-limit"),
        Line::from("- f follow active session"),
        Line::from(""),
        Line::from("q quit, ? toggle this help"),
    ];

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::default()
                .title("Help")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        popup,
    );
}

fn draw_settings_overlay(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let popup = centered_rect(58, 72, frame.area());
    frame.render_widget(Clear, popup);

    let defaults = TuiSettings::default();
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::DarkGray);
    let separator_idx = 5;

    // --- General settings rows ---
    struct SettingRow {
        label: &'static str,
        value: String,
        is_default: bool,
    }

    let general_rows = [
        SettingRow {
            label: "Profile",
            value: format!("{}", state.settings.default_profile),
            is_default: state.settings.default_profile == defaults.default_profile,
        },
        SettingRow {
            label: "Refresh Interval",
            value: format!("{}ms", state.settings.auto_refresh_ms),
            is_default: state.settings.auto_refresh_ms == defaults.auto_refresh_ms,
        },
        SettingRow {
            label: "Session Limit",
            value: format!("{}", state.settings.session_limit),
            is_default: state.settings.session_limit == defaults.session_limit,
        },
        SettingRow {
            label: "Run Limit",
            value: format!("{}", state.settings.run_limit),
            is_default: state.settings.run_limit == defaults.run_limit,
        },
        SettingRow {
            label: "Follow Active",
            value: format!("{}", state.settings.follow_active_session),
            is_default: state.settings.follow_active_session == defaults.follow_active_session,
        },
    ];

    let mut lines = Vec::<Line>::new();
    lines.push(Line::from(""));

    for (i, row) in general_rows.iter().enumerate() {
        let is_selected = i == state.settings_selected;
        let value_style = if !row.is_default {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let (left_arrow, right_arrow) = if is_selected {
            (
                Span::styled(" \u{25C0} ", key_style),
                Span::styled(" \u{25B6}", key_style),
            )
        } else {
            (
                Span::styled("   ", Style::default()),
                Span::styled("  ", Style::default()),
            )
        };

        let label_style = if is_selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {:<22}", row.label), label_style),
            left_arrow,
            Span::styled(format!("{:<14}", row.value), value_style),
            right_arrow,
        ]));
    }

    // --- Separator ---
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  \u{2500}\u{2500} Models \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ]));

    // --- Model toggle rows ---
    for (i, info) in state.model_list.iter().enumerate() {
        let row_idx = separator_idx + 1 + i;
        let is_selected = row_idx == state.settings_selected;
        let is_disabled = state
            .settings
            .disabled_models
            .contains(&info.model_id);
        let (status_text, status_style) = if is_disabled {
            (
                "OFF",
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                "ON ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        };

        let (left_arrow, right_arrow) = if is_selected {
            (
                Span::styled(" \u{25C0} ", key_style),
                Span::styled(" \u{25B6}", key_style),
            )
        } else {
            (
                Span::styled("   ", Style::default()),
                Span::styled("  ", Style::default()),
            )
        };

        let label_style = if is_selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if is_disabled {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(Line::from(vec![
            Span::styled(format!("  {:<22}", info.display_name), label_style),
            left_arrow,
            Span::styled(status_text.to_string(), status_style),
            Span::styled(format!("{:<11}", ""), Style::default()),
            right_arrow,
        ]));
    }

    // --- Footer ---
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}",
            desc_style,
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  j/k", key_style),
        Span::styled(" navigate  ", desc_style),
        Span::styled("h/l", key_style),
        Span::styled(" toggle  ", desc_style),
        Span::styled("R", key_style),
        Span::styled(" reset  ", desc_style),
        Span::styled("Esc", key_style),
        Span::styled(" close", desc_style),
    ]));

    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(Span::styled(
                    " Settings ",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta)),
        ),
        popup,
    );
}

fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    area: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn pane_label(pane: FocusPane) -> &'static str {
    match pane {
        FocusPane::Sessions => "sessions",
        FocusPane::Runs => "runs",
        FocusPane::Details => "details",
    }
}

fn run_status_style(status: &str) -> Style {
    match status {
        "running" => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
        "paused" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "cancelling" | "cancelled" => Style::default().fg(Color::Gray),
        "succeeded" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "failed" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn matches_session_filter(session: &SessionSummary, filter: &str) -> bool {
    let term = filter.trim().to_lowercase();
    if term.is_empty() {
        return true;
    }
    let session_id = session.session_id.to_string();
    let last = session
        .last_task
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    session_id.contains(term.as_str())
        || short_uuid(session.session_id).contains(term.as_str())
        || last.contains(term.as_str())
}

fn matches_run_filter(run: &RunRecord, filter: &str) -> bool {
    let term = filter.trim().to_lowercase();
    if term.is_empty() {
        return true;
    }
    let run_id = run.run_id.to_string();
    let task = run.task.to_lowercase();
    let status = run.status.to_string();
    run_id.contains(term.as_str())
        || short_uuid(run.run_id).contains(term.as_str())
        || task.contains(term.as_str())
        || status.contains(term.as_str())
}

fn status_marker(status: &str) -> &'static str {
    match status {
        "running" => "[RUN]",
        "paused" => "[PAU]",
        "cancelling" => "[CNG]",
        "cancelled" => "[CXL]",
        "succeeded" => "[OK ]",
        "failed" => "[ERR]",
        "skipped" => "[SKIP]",
        _ => "[WAIT]",
    }
}

fn build_timeline_lines(trace: &RunTrace, width: usize, max_nodes: usize) -> Vec<Line<'static>> {
    let mut nodes = trace.graph.nodes.clone();
    nodes.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    let now = Utc::now();
    let starts = nodes
        .iter()
        .filter_map(|n| n.started_at)
        .collect::<Vec<_>>();
    if starts.is_empty() {
        return vec![Line::from("timeline unavailable (no node runtime yet)")];
    }

    let window_start = starts.into_iter().min().unwrap_or(now);
    let mut window_end = window_start;
    for node in &nodes {
        let Some(started_at) = node.started_at else {
            continue;
        };
        let estimated_end = if let Some(finished_at) = node.finished_at {
            finished_at
        } else if let Some(duration_ms) = node.duration_ms {
            let bounded = duration_ms.min(i64::MAX as u128) as i64;
            started_at + chrono::Duration::milliseconds(bounded)
        } else {
            now
        };
        if estimated_end > window_end {
            window_end = estimated_end;
        }
    }

    if window_end <= window_start {
        window_end = window_start + chrono::Duration::milliseconds(1);
    }

    let total_ms = window_end
        .signed_duration_since(window_start)
        .num_milliseconds()
        .max(1) as f64;

    let mut lines = Vec::new();
    for node in nodes.iter().take(max_nodes) {
        let name = compact_json(node.node_id.as_str(), 12);
        let mut lane = vec!['.'; width];

        if let Some(started_at) = node.started_at {
            let end_at = node.finished_at.unwrap_or(now);
            let mut start_idx = (((started_at
                .signed_duration_since(window_start)
                .num_milliseconds()
                .max(0) as f64)
                / total_ms)
                * (width.saturating_sub(1) as f64))
                .round() as usize;
            let mut end_idx = (((end_at
                .signed_duration_since(window_start)
                .num_milliseconds()
                .max(0) as f64)
                / total_ms)
                * (width.saturating_sub(1) as f64))
                .round() as usize;

            if start_idx >= width {
                start_idx = width.saturating_sub(1);
            }
            if end_idx >= width {
                end_idx = width.saturating_sub(1);
            }
            if end_idx < start_idx {
                end_idx = start_idx;
            }

            let marker = lane_marker(node.status.as_str());
            for slot in lane.iter_mut().take(end_idx + 1).skip(start_idx) {
                *slot = marker;
            }
        }

        let duration = node.duration_ms.unwrap_or(0);
        lines.push(Line::from(format!(
            "{} {:<12} |{}| {}ms",
            status_marker(node.status.as_str()),
            name,
            lane.into_iter().collect::<String>(),
            duration
        )));
    }

    if nodes.len() > max_nodes {
        lines.push(Line::from(format!("... +{}", nodes.len() - max_nodes)));
    }

    lines
}

fn build_action_mix(trace: &RunTrace, top_n: usize) -> Vec<(String, usize)> {
    let mut counts = HashMap::<String, usize>::new();
    for event in &trace.events {
        let label = event.action.to_string();
        let next = counts.get(&label).copied().unwrap_or(0) + 1;
        counts.insert(label, next);
    }

    let mut items = counts.into_iter().collect::<Vec<_>>();
    items.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    items.into_iter().take(top_n).collect()
}

fn lane_marker(status: &str) -> char {
    match status {
        "succeeded" => '=',
        "failed" => '!',
        "running" => '>',
        "paused" => ':',
        "cancelling" => '~',
        "cancelled" => 'x',
        "skipped" => '-',
        _ => '.',
    }
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

fn cycle_profile_back(current: TaskProfile) -> TaskProfile {
    match current {
        TaskProfile::Planning => TaskProfile::General,
        TaskProfile::Extraction => TaskProfile::Planning,
        TaskProfile::Coding => TaskProfile::Extraction,
        TaskProfile::General => TaskProfile::Coding,
    }
}

fn toggle_model(state: &mut TuiState, orchestrator: &Orchestrator, model_idx: usize) {
    if let Some(info) = state.model_list.get(model_idx) {
        let mid = &info.model_id;
        let currently_disabled = state.settings.disabled_models.contains(mid);
        if currently_disabled {
            state.settings.disabled_models.retain(|m| m != mid);
            orchestrator.router().set_model_disabled(mid, false);
            state.set_status(format!("{} enabled", info.display_name));
        } else {
            state.settings.disabled_models.push(mid.clone());
            orchestrator.router().set_model_disabled(mid, true);
            state.set_status(format!("{} disabled", info.display_name));
        }
    }
}

fn parse_provider_kind(s: &str) -> Option<ProviderKind> {
    match s {
        "openai" => Some(ProviderKind::OpenAi),
        "anthropic" => Some(ProviderKind::Anthropic),
        "gemini" => Some(ProviderKind::Gemini),
        "vllm" => Some(ProviderKind::Vllm),
        "mock" => Some(ProviderKind::Mock),
        _ => None,
    }
}

fn relative_time(dt: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let diff = now.signed_duration_since(dt);
    let secs = diff.num_seconds();
    if secs < 0 {
        return "just now".to_string();
    }
    if secs < 60 {
        return format!("{}s ago", secs);
    }
    let mins = diff.num_minutes();
    if mins < 60 {
        return format!("{}m ago", mins);
    }
    let hours = diff.num_hours();
    if hours < 24 {
        return format!("{}h ago", hours);
    }
    let days = diff.num_days();
    format!("{}d ago", days)
}

fn format_duration_ms(ms: u128) -> String {
    if ms < 1_000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        let secs = ms / 1_000;
        format!("{}m{}s", secs / 60, secs % 60)
    }
}

fn pane_block(title: String, pane: FocusPane, current_focus: FocusPane) -> Block<'static> {
    let is_focused = pane == current_focus;
    let border_color = if is_focused {
        match pane {
            FocusPane::Sessions => Color::Cyan,
            FocusPane::Runs => Color::Green,
            FocusPane::Details => Color::Yellow,
        }
    } else {
        Color::DarkGray
    };
    let title_style = if is_focused {
        Style::default()
            .fg(border_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, title_style))
}
