use crate::client;
use crate::paths::Paths;
use crate::protocol::{ProcessInfo, ProcessStatus, Request, Response};
use color_eyre::eyre::Context;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::terminal::{Frame, Terminal};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use std::io;
use std::time::{Duration, Instant};

const TICK_RATE: Duration = Duration::from_millis(1000);

pub fn run(paths: &Paths) -> color_eyre::Result<()> {
    let _guard = TerminalGuard::new()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("failed to initialize terminal")?;
    terminal.clear().context("failed to clear terminal")?;

    let mut app = App::new();
    app.refresh(paths);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        let timeout = TICK_RATE.saturating_sub(last_tick.elapsed());
        if event::poll(timeout).context("failed to poll terminal events")?
            && let Event::Key(key) = event::read().context("failed to read terminal event")?
            && key.kind == KeyEventKind::Press
            && handle_key_event(&mut app, key.code, key.modifiers)
        {
            break;
        }

        if last_tick.elapsed() >= TICK_RATE {
            app.refresh(paths);
            last_tick = Instant::now();
        }
    }

    terminal.show_cursor().ok();
    Ok(())
}

fn handle_key_event(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Down | KeyCode::Char('j') => app.next(),
        KeyCode::Up | KeyCode::Char('k') => app.previous(),
        KeyCode::Home => app.first(),
        KeyCode::End => app.last(),
        _ => {}
    }
    false
}

fn ui(f: &mut Frame, app: &mut App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10),
            Constraint::Min(7),
            Constraint::Length(1),
        ])
        .split(f.size());

    let header = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(layout[0]);

    let banner = banner_widget(app);
    f.render_widget(banner, header[0]);

    let header_body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(20)])
        .split(header[1]);

    f.render_widget(logo_widget(), header_body[0]);
    let counts = status_counts(&app.processes);
    f.render_widget(status_widget(app, &counts), header_body[1]);

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(layout[1]);

    let header = Row::new(vec![
        Cell::from("name"),
        Cell::from("pid"),
        Cell::from("status"),
        Cell::from("uptime"),
        Cell::from("restarts"),
        Cell::from("cpu"),
        Cell::from("mem"),
    ])
    .style(
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let rows = app.processes.iter().enumerate().map(|(idx, p)| {
        let pid = p
            .pid
            .map(|id| id.to_string())
            .unwrap_or_else(|| "-".to_string());
        let uptime = format_uptime(p.uptime);
        let restarts = p.restarts.to_string();
        let cpu = format_cpu(p.cpu_percent);
        let mem = format_memory(p.memory_bytes);
        let status = p.status.to_string();

        let restarts_style = if p.restarts > 0 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };

        let row_style = if idx % 2 == 0 {
            Style::default()
        } else {
            Style::default().bg(Color::Rgb(18, 18, 18))
        };

        Row::new(vec![
            Cell::from(p.name.clone()).style(Style::default().fg(Color::Cyan)),
            Cell::from(pid),
            Cell::from(status).style(status_style(p.status)),
            Cell::from(uptime),
            Cell::from(restarts).style(restarts_style),
            Cell::from(cpu),
            Cell::from(mem),
        ])
        .style(row_style)
    });

    let widths = [
        Constraint::Min(14),
        Constraint::Length(7),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(10),
    ];

    let title = format!("Processes ({})", app.processes.len());
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::LightBlue)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(table, main[0], &mut app.table_state);
    let details = details_widget(app.selected_process());
    f.render_widget(details, main[1]);

    let footer = if let Some(err) = &app.last_error {
        format!("q quit | ↑/↓ move | error: {err}")
    } else {
        "q quit | ↑/↓ move".to_string()
    };
    let footer = Paragraph::new(footer)
        .block(Block::default().borders(Borders::TOP))
        .style(Style::default().fg(Color::DarkGray));
    f.render_widget(footer, layout[2]);
}

fn banner_widget(app: &App) -> Paragraph<'static> {
    let bar_style = Style::default().bg(Color::DarkGray).fg(Color::White);
    let status_style = if app.last_error.is_some() {
        Style::default().bg(Color::DarkGray).fg(Color::Red)
    } else {
        Style::default().bg(Color::DarkGray).fg(Color::Green)
    };
    let status_label = if app.last_error.is_some() {
        "disconnected"
    } else {
        "connected"
    };

    let line = Line::from(vec![
        Span::styled(" PM3 TUI ", bar_style.add_modifier(Modifier::BOLD)),
        Span::styled(" | ", bar_style),
        Span::styled("processes: ", bar_style),
        Span::styled(app.processes.len().to_string(), bar_style),
        Span::styled(" | ", bar_style),
        Span::styled("daemon: ", bar_style),
        Span::styled(status_label, status_style.add_modifier(Modifier::BOLD)),
        Span::styled(" | refresh 1s | q quit ", bar_style),
    ]);

    Paragraph::new(Text::from(line))
        .block(Block::default())
        .style(bar_style)
}

fn logo_widget() -> Paragraph<'static> {
    let main_style = Style::default()
        .fg(Color::LightCyan)
        .add_modifier(Modifier::BOLD);
    let shadow_style = Style::default().fg(Color::DarkGray);
    let accent = Style::default().fg(Color::LightBlue);

    let logo = [
        " ____   __  __  _____ ",
        "|  _ \\ |  \\/  ||___ / ",
        "| |_) || |\\/| |  |_ \\ ",
        "|  __/ | |  | | ___) |",
        "|_|    |_|  |_||____/ ",
    ];

    let mut lines = Vec::with_capacity(logo.len() + 1);
    for line in logo {
        let shadow = shadow_line(line);
        lines.push(Line::from(vec![
            Span::styled(line, main_style),
            Span::raw(" "),
            Span::styled(shadow, shadow_style),
        ]));
    }
    lines.push(Line::from(Span::styled("    process manager", accent)));

    Paragraph::new(Text::from(lines)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    )
}

fn status_widget(app: &App, counts: &StatusCounts) -> Paragraph<'static> {
    let label_style = Style::default().fg(Color::Gray);
    let value_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let lines = vec![
        Line::from(Span::styled(
            "LIVE STATUS",
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        status_line("online", counts.online, status_style(ProcessStatus::Online)),
        status_line(
            "starting",
            counts.starting,
            status_style(ProcessStatus::Starting),
        ),
        status_line(
            "unhealthy",
            counts.unhealthy,
            status_style(ProcessStatus::Unhealthy),
        ),
        status_line(
            "stopped",
            counts.stopped,
            status_style(ProcessStatus::Stopped),
        ),
        status_line(
            "errored",
            counts.errored,
            status_style(ProcessStatus::Errored),
        ),
        Line::from(""),
        Line::from(vec![
            Span::styled("total", label_style),
            Span::raw("  "),
            Span::styled(app.processes.len().to_string(), value_style),
        ]),
    ];

    Paragraph::new(Text::from(lines)).block(
        Block::default()
            .title("Summary")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    )
}

fn status_line(label: &'static str, count: usize, style: Style) -> Line<'static> {
    let label_style = Style::default().fg(Color::Gray);
    let value_style = style.add_modifier(Modifier::BOLD);
    let label_padded = format!("{label:<9}");
    Line::from(vec![
        Span::styled(label_padded, label_style),
        Span::styled(count.to_string(), value_style),
    ])
}

fn details_widget(selected: Option<&ProcessInfo>) -> Paragraph<'static> {
    let label_style = Style::default().fg(Color::Gray);
    let mut lines = Vec::new();

    if let Some(p) = selected {
        lines.push(Line::from(vec![
            Span::styled("name ", label_style),
            Span::styled(p.name.clone(), Style::default().fg(Color::Cyan)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("status ", label_style),
            Span::styled(p.status.to_string(), status_style(p.status)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("pid ", label_style),
            Span::raw(
                p.pid
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("uptime ", label_style),
            Span::raw(format_uptime(p.uptime)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("restarts ", label_style),
            Span::raw(p.restarts.to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("cpu ", label_style),
            Span::raw(format_cpu(p.cpu_percent)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("mem ", label_style),
            Span::raw(format_memory(p.memory_bytes)),
        ]));
        if let Some(group) = &p.group {
            lines.push(Line::from(vec![
                Span::styled("group ", label_style),
                Span::raw(group.clone()),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled("no process selected", label_style)));
    }

    Paragraph::new(Text::from(lines)).block(
        Block::default()
            .title("Selected")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    )
}

fn shadow_line(line: &str) -> String {
    line.chars()
        .map(|ch| if ch == ' ' { ' ' } else { '.' })
        .collect()
}

fn status_style(status: ProcessStatus) -> Style {
    match status {
        ProcessStatus::Online => Style::default().fg(Color::Green),
        ProcessStatus::Starting => Style::default().fg(Color::Yellow),
        ProcessStatus::Unhealthy => Style::default().fg(Color::Magenta),
        ProcessStatus::Stopped => Style::default().fg(Color::Gray),
        ProcessStatus::Errored => Style::default().fg(Color::Red),
    }
}

fn format_uptime(seconds: Option<u64>) -> String {
    match seconds {
        None => "-".to_string(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        Some(s) if s < 86400 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        Some(s) => format!("{}d {}h", s / 86400, (s % 86400) / 3600),
    }
}

fn format_cpu(cpu: Option<f64>) -> String {
    cpu.map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "-".to_string())
}

fn format_memory(bytes: Option<u64>) -> String {
    let bytes = match bytes {
        Some(value) => value,
        None => return "-".to_string(),
    };

    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut idx = 0;

    while value >= 1024.0 && idx < units.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }

    if idx == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", units[idx])
    }
}

#[derive(Default)]
struct StatusCounts {
    online: usize,
    starting: usize,
    unhealthy: usize,
    stopped: usize,
    errored: usize,
}

fn status_counts(processes: &[ProcessInfo]) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for p in processes {
        match p.status {
            ProcessStatus::Online => counts.online += 1,
            ProcessStatus::Starting => counts.starting += 1,
            ProcessStatus::Unhealthy => counts.unhealthy += 1,
            ProcessStatus::Stopped => counts.stopped += 1,
            ProcessStatus::Errored => counts.errored += 1,
        }
    }
    counts
}

struct App {
    processes: Vec<ProcessInfo>,
    table_state: TableState,
    last_error: Option<String>,
}

impl App {
    fn new() -> Self {
        Self {
            processes: Vec::new(),
            table_state: TableState::default(),
            last_error: None,
        }
    }

    fn refresh(&mut self, paths: &Paths) {
        match client::send_request(paths, &Request::List) {
            Ok(Response::ProcessList { processes }) => {
                self.last_error = None;
                self.set_processes(processes);
            }
            Ok(other) => {
                self.last_error = Some(format!("unexpected response: {other:?}"));
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
    }

    fn set_processes(&mut self, processes: Vec<ProcessInfo>) {
        let selected_name = self.selected_name().map(|name| name.to_string());
        self.processes = processes;

        if self.processes.is_empty() {
            self.table_state.select(None);
            return;
        }

        if let Some(name) = selected_name
            && let Some(idx) = self.processes.iter().position(|p| p.name == name)
        {
            self.table_state.select(Some(idx));
            return;
        }

        let idx = self
            .table_state
            .selected()
            .filter(|&i| i < self.processes.len())
            .unwrap_or(0);
        self.table_state.select(Some(idx));
    }

    fn selected_name(&self) -> Option<&str> {
        self.table_state
            .selected()
            .and_then(|idx| self.processes.get(idx))
            .map(|p| p.name.as_str())
    }

    fn selected_process(&self) -> Option<&ProcessInfo> {
        self.table_state
            .selected()
            .and_then(|idx| self.processes.get(idx))
    }

    fn next(&mut self) {
        let len = self.processes.len();
        if len == 0 {
            return;
        }
        let next = match self.table_state.selected() {
            Some(i) if i + 1 < len => i + 1,
            _ => 0,
        };
        self.table_state.select(Some(next));
    }

    fn previous(&mut self) {
        let len = self.processes.len();
        if len == 0 {
            return;
        }
        let prev = match self.table_state.selected() {
            Some(0) | None => len - 1,
            Some(i) => i - 1,
        };
        self.table_state.select(Some(prev));
    }

    fn first(&mut self) {
        if !self.processes.is_empty() {
            self.table_state.select(Some(0));
        }
    }

    fn last(&mut self) {
        if !self.processes.is_empty() {
            self.table_state.select(Some(self.processes.len() - 1));
        }
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn new() -> color_eyre::Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)
            .context("failed to enter alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
    }
}
