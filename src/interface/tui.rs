use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Terminal;

use crate::orchestrator::Orchestrator;

pub async fn run_tui(orchestrator: Orchestrator) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut last_refresh = Instant::now() - Duration::from_secs(5);
    let mut cached_runs = Vec::new();
    let mut status_text = "loading...".to_string();

    let result = (|| -> anyhow::Result<()> {
        loop {
            if last_refresh.elapsed() >= Duration::from_secs(1) {
                match futures::executor::block_on(orchestrator.list_recent_runs(20)) {
                    Ok(runs) => {
                        cached_runs = runs;
                        status_text = format!("{} runs loaded", cached_runs.len());
                    }
                    Err(err) => {
                        status_text = format!("failed to fetch runs: {}", err);
                    }
                }
                last_refresh = Instant::now();
            }

            terminal.draw(|frame| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Min(8),
                        Constraint::Length(3),
                    ])
                    .split(frame.size());

                let header = Paragraph::new("CLI Agent TUI - press q to quit")
                    .block(Block::default().borders(Borders::ALL).title("Agent"));
                frame.render_widget(header, chunks[0]);

                let items = cached_runs
                    .iter()
                    .map(|run| {
                        ListItem::new(format!(
                            "{} | {} | {} | {}",
                            run.created_at.to_rfc3339(),
                            run.run_id,
                            run.status,
                            run.task
                        ))
                    })
                    .collect::<Vec<_>>();

                let runs = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title("Recent Runs"));
                frame.render_widget(runs, chunks[1]);

                let footer = Paragraph::new(status_text.clone())
                    .style(Style::default().fg(Color::Yellow))
                    .block(Block::default().borders(Borders::ALL).title("Status"));
                frame.render_widget(footer, chunks[2]);
            })?;

            let polled = tokio::task::block_in_place(|| event::poll(Duration::from_millis(100)))?;
            if polled {
                let ev = tokio::task::block_in_place(event::read)?;
                if let Event::Key(key) = ev {
                    if key.code == KeyCode::Char('q') {
                        break;
                    }
                }
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}
