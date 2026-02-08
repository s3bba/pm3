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
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::terminal::{Frame, Terminal};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Paragraph, Row, Table, TableState, block::Padding,
};
use std::io;
use std::time::{Duration, Instant};

const TICK_RATE: Duration = Duration::from_millis(1000);

// ── Color palette (cyan / green / red) ───────────────────────────────
const BG_BASE: Color = Color::Reset;
const BG_SURFACE: Color = Color::Reset;
const BG_HIGHLIGHT: Color = Color::DarkGray;

const FG_DIM: Color = Color::DarkGray;
const FG_MUTED: Color = Color::DarkGray;
const FG_TEXT: Color = Color::White;
const FG_BRIGHT: Color = Color::White;

const ACCENT: Color = Color::Green;
const ACCENT_DIM: Color = Color::DarkGray;

const STATUS_GREEN: Color = Color::Green;
const STATUS_YELLOW: Color = Color::Green;
const STATUS_MAGENTA: Color = Color::Red;
const STATUS_GRAY: Color = Color::DarkGray;
const STATUS_RED: Color = Color::Red;

const KEY_BG: Color = Color::Black;
const KEY_FG: Color = Color::Green;

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
    let terminal_width = f.size().width;

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header card
            Constraint::Length(1), // spacer
            Constraint::Min(5),    // table
            Constraint::Length(1), // spacer
            Constraint::Length(1), // footer
        ])
        .split(f.size());

    render_header(f, app, layout[0]);

    let spacer = Paragraph::new("").style(Style::default().bg(BG_BASE));
    f.render_widget(spacer.clone(), layout[1]);
    f.render_widget(spacer, layout[3]);

    render_table(f, app, layout[2], terminal_width);
    render_footer(f, app, layout[4]);
}

fn render_header(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let (dot, dot_label) = if app.last_error.is_some() {
        (
            Span::styled("● ", Style::default().fg(STATUS_RED)),
            Span::styled(
                "disconnected",
                Style::default().fg(STATUS_RED).add_modifier(Modifier::BOLD),
            ),
        )
    } else {
        (
            Span::styled("● ", Style::default().fg(STATUS_GREEN)),
            Span::styled(
                "connected",
                Style::default()
                    .fg(STATUS_GREEN)
                    .add_modifier(Modifier::BOLD),
            ),
        )
    };

    let banner_line = Line::from(vec![
        Span::styled(
            " PM3 ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ─ ", Style::default().fg(FG_DIM)),
        Span::styled("daemon: ", Style::default().fg(FG_MUTED)),
        dot,
        dot_label,
        Span::styled(" ─ ", Style::default().fg(FG_DIM)),
        Span::styled("processes: ", Style::default().fg(FG_MUTED)),
        Span::styled(
            app.processes.len().to_string(),
            Style::default().fg(FG_TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ─ ", Style::default().fg(FG_DIM)),
        Span::styled("refresh 1s", Style::default().fg(FG_DIM)),
    ]);

    let counts = status_counts(&app.processes);
    let label = Style::default().fg(FG_DIM);
    let val = |color: Color| Style::default().fg(color).add_modifier(Modifier::BOLD);

    let status_line = Line::from(vec![
        Span::styled(" ", label),
        status_dot(ProcessStatus::Online),
        Span::styled("online: ", label),
        Span::styled(counts.online.to_string(), val(STATUS_GREEN)),
        Span::styled("   ", label),
        status_dot(ProcessStatus::Starting),
        Span::styled("starting: ", label),
        Span::styled(counts.starting.to_string(), val(STATUS_YELLOW)),
        Span::styled("   ", label),
        status_dot(ProcessStatus::Unhealthy),
        Span::styled("unhealthy: ", label),
        Span::styled(counts.unhealthy.to_string(), val(STATUS_MAGENTA)),
        Span::styled("   ", label),
        status_dot(ProcessStatus::Stopped),
        Span::styled("stopped: ", label),
        Span::styled(counts.stopped.to_string(), val(STATUS_GRAY)),
        Span::styled("   ", label),
        status_dot(ProcessStatus::Errored),
        Span::styled("errored: ", label),
        Span::styled(counts.errored.to_string(), val(STATUS_RED)),
        Span::styled("   ", label),
        Span::styled("total: ", label),
        Span::styled(
            app.processes.len().to_string(),
            Style::default().fg(FG_TEXT).add_modifier(Modifier::BOLD),
        ),
    ]);

    let header_text = Text::from(vec![banner_line, Line::from(""), status_line]);
    let header = Paragraph::new(header_text).style(Style::default().bg(BG_SURFACE));
    f.render_widget(header, area);
}

fn status_dot(status: ProcessStatus) -> Span<'static> {
    let color = match status {
        ProcessStatus::Online => STATUS_GREEN,
        ProcessStatus::Starting => STATUS_YELLOW,
        ProcessStatus::Unhealthy => STATUS_MAGENTA,
        ProcessStatus::Stopped => STATUS_GRAY,
        ProcessStatus::Errored => STATUS_RED,
    };
    Span::styled("● ", Style::default().fg(color))
}

fn render_table(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect, terminal_width: u16) {
    let selected_idx = app.table_state.selected();

    // Determine column layout based on terminal width
    let is_wide = terminal_width >= 100;
    let is_medium = terminal_width >= 70;

    let header_cells: Vec<Cell> = if is_wide {
        vec![
            Cell::from(Line::from("NAME")),
            Cell::from(Line::from("PID").alignment(Alignment::Right)),
            Cell::from(Line::from("STATUS")),
            Cell::from(Line::from("UPTIME")),
            Cell::from(Line::from("RESTARTS").alignment(Alignment::Right)),
            Cell::from(Line::from("CPU").alignment(Alignment::Right)),
            Cell::from(Line::from("MEM").alignment(Alignment::Right)),
        ]
    } else if is_medium {
        vec![
            Cell::from(Line::from("NAME")),
            Cell::from(Line::from("PID").alignment(Alignment::Right)),
            Cell::from(Line::from("STATUS")),
            Cell::from(Line::from("UPTIME")),
            Cell::from(Line::from("RESTARTS").alignment(Alignment::Right)),
        ]
    } else {
        vec![
            Cell::from(Line::from("NAME")),
            Cell::from(Line::from("STATUS")),
            Cell::from(Line::from("UPTIME")),
        ]
    };

    let header =
        Row::new(header_cells).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));

    let rows = app.processes.iter().enumerate().map(|(idx, p)| {
        let pid = p
            .pid
            .map(|id| id.to_string())
            .unwrap_or_else(|| "-".to_string());
        let uptime = format_uptime(p.uptime);
        let restarts = p.restarts.to_string();
        let cpu = format_cpu(p.cpu_percent);
        let mem = format_memory(p.memory_bytes);

        let restarts_style = if p.restarts > 0 {
            Style::default().fg(STATUS_YELLOW)
        } else {
            Style::default().fg(FG_TEXT)
        };

        let is_selected = selected_idx == Some(idx);
        let row_bg = if is_selected {
            BG_HIGHLIGHT
        } else if idx % 2 == 0 {
            BG_BASE
        } else {
            BG_SURFACE
        };
        let row_fg = if is_selected { FG_BRIGHT } else { FG_TEXT };

        let name_display = if is_selected {
            format!("▸ {}", p.name)
        } else {
            format!("  {}", p.name)
        };

        let status_line = Line::from(vec![
            status_dot(p.status),
            Span::styled(p.status.to_string(), status_style(p.status)),
        ]);

        let cells: Vec<Cell> = if is_wide {
            vec![
                Cell::from(Line::from(Span::styled(
                    name_display,
                    Style::default().fg(ACCENT),
                ))),
                Cell::from(Line::from(pid).alignment(Alignment::Right)),
                Cell::from(status_line),
                Cell::from(Line::from(uptime)),
                Cell::from(
                    Line::from(Span::styled(restarts, restarts_style)).alignment(Alignment::Right),
                ),
                Cell::from(Line::from(cpu).alignment(Alignment::Right)),
                Cell::from(Line::from(mem).alignment(Alignment::Right)),
            ]
        } else if is_medium {
            vec![
                Cell::from(Line::from(Span::styled(
                    name_display,
                    Style::default().fg(ACCENT),
                ))),
                Cell::from(Line::from(pid).alignment(Alignment::Right)),
                Cell::from(status_line),
                Cell::from(Line::from(uptime)),
                Cell::from(
                    Line::from(Span::styled(restarts, restarts_style)).alignment(Alignment::Right),
                ),
            ]
        } else {
            vec![
                Cell::from(Line::from(Span::styled(
                    name_display,
                    Style::default().fg(ACCENT),
                ))),
                Cell::from(status_line),
                Cell::from(Line::from(uptime)),
            ]
        };

        Row::new(cells).style(Style::default().bg(row_bg).fg(row_fg))
    });

    let widths: Vec<Constraint> = if is_wide {
        vec![
            Constraint::Percentage(25),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(10),
        ]
    } else if is_medium {
        vec![
            Constraint::Min(16),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(9),
        ]
    } else {
        vec![
            Constraint::Min(16),
            Constraint::Length(12),
            Constraint::Length(10),
        ]
    };

    let title = format!(" Processes ({}) ", app.processes.len());
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(Span::styled(
                    title,
                    Style::default().fg(FG_TEXT).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACCENT_DIM))
                .padding(Padding::horizontal(1)),
        )
        .highlight_style(Style::default())
        .highlight_symbol("");

    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn render_footer(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let key_style = Style::default()
        .fg(KEY_FG)
        .bg(KEY_BG)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(FG_DIM);

    let mut spans = vec![
        Span::styled(" ", label_style),
        Span::styled(" q ", key_style),
        Span::styled(" quit ", label_style),
        Span::styled(" ↑↓ ", key_style),
        Span::styled(" move ", label_style),
        Span::styled(" Home ", key_style),
        Span::styled(" first ", label_style),
        Span::styled(" End ", key_style),
        Span::styled(" last ", label_style),
    ];

    if let Some(err) = &app.last_error {
        spans.push(Span::styled("  ", label_style));
        spans.push(Span::styled(
            format!(" error: {err} "),
            Style::default()
                .fg(FG_BRIGHT)
                .bg(STATUS_RED)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let line = Line::from(spans);
    let footer = Paragraph::new(Text::from(line)).style(Style::default().bg(BG_BASE));
    f.render_widget(footer, area);
}

fn status_style(status: ProcessStatus) -> Style {
    match status {
        ProcessStatus::Online => Style::default().fg(STATUS_GREEN),
        ProcessStatus::Starting => Style::default().fg(STATUS_YELLOW),
        ProcessStatus::Unhealthy => Style::default().fg(STATUS_MAGENTA),
        ProcessStatus::Stopped => Style::default().fg(STATUS_GRAY),
        ProcessStatus::Errored => Style::default().fg(STATUS_RED),
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
