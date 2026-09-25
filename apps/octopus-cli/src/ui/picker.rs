use std::io;

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
};
use std::io::stdout;

use crate::session::Session;

pub fn pick_session_interactive(sessions: Vec<Session>) -> io::Result<Option<String>> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_session_picker(&mut terminal, sessions);

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;

    result
}

pub struct SessionPicker {
    sessions: Vec<Session>,
    state: ListState,
}

impl SessionPicker {
    pub fn new(sessions: Vec<Session>) -> Self {
        let mut state = ListState::default();
        if !sessions.is_empty() {
            state.select(Some(0));
        }
        Self { sessions, state }
    }

    pub fn selected(&self) -> Option<String> {
        self.state.selected().map(|i| self.sessions[i].id.clone())
    }

    fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.sessions.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.sessions.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }
}

pub fn run_session_picker<B: Backend>(
    terminal: &mut Terminal<B>,
    sessions: Vec<Session>,
) -> io::Result<Option<String>> {
    let mut picker = SessionPicker::new(sessions);
    let mut cancelled = false;

    loop {
        terminal.draw(|f| draw(f, &mut picker))?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    cancelled = true;
                    break;
                }
                KeyCode::Enter => break,
                KeyCode::Down | KeyCode::Char('j') => picker.next(),
                KeyCode::Up | KeyCode::Char('k') => picker.previous(),
                _ => {}
            }
        }
    }

    if cancelled {
        Ok(None)
    } else {
        Ok(picker.selected())
    }
}

fn draw(frame: &mut Frame, picker: &mut SessionPicker) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([Constraint::Min(3)])
        .split(frame.area());

    let items: Vec<ListItem> = picker
        .sessions
        .iter()
        .map(|s| {
            let id_short = &s.id[..8.min(s.id.len())];
            let title = if s.title.is_empty() {
                "Untitled".to_string()
            } else {
                s.title.clone()
            };
            let updated = format_time(s.updated_at);
            let text = Line::from(vec![
                Span::styled(
                    format!("{}  ", id_short),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(title, Style::default().fg(Color::White)),
                Span::styled(
                    format!("  ({})", updated),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            ListItem::new(text)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Sessions ")
                .title_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, chunks[0], &mut picker.state);

    let help = Line::from(vec![Span::styled(
        "↑/k ↓/j  navigate  •  Enter  select  •  q/Esc  cancel",
        Style::default().fg(Color::DarkGray),
    )]);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(help),
        ratatui::layout::Rect {
            x: chunks[0].x,
            y: frame.area().height.saturating_sub(1),
            width: chunks[0].width,
            height: 1,
        },
    );
}

fn format_time(timestamp: f64) -> String {
    if timestamp <= 0.0 {
        return "unknown".to_string();
    }
    let dt = chrono::DateTime::from_timestamp(timestamp as i64, 0);
    match dt {
        Some(d) => d.format("%Y-%m-%d %H:%M").to_string(),
        None => "unknown".to_string(),
    }
}
