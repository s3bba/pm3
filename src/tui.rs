use crate::client;
use crate::config;
use crate::log;
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
use ratatui::{Frame, Terminal};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Padding, Paragraph, Row, Table, TableState,
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
const STATUS_GRAY: Color = Color::Yellow;
const STATUS_RED: Color = Color::Red;

const KEY_BG: Color = Color::Black;
const KEY_FG: Color = Color::Green;

// ── View types ──────────────────────────────────────────────────────
enum LogSource {
    Stdout,
    Stderr,
}

enum View {
    ProcessList,
    LogViewer {
        process_name: String,
        lines: Vec<String>,
        scroll_offset: u16,
        auto_scroll: bool,
        stream: LogSource,
    },
}

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
            && handle_key_event(&mut app, key.code, key.modifiers, paths)
        {
            break;
        }

        if last_tick.elapsed() >= TICK_RATE {
            app.refresh(paths);
            app.refresh_logs(paths);
            last_tick = Instant::now();
        }
    }

    terminal.show_cursor().ok();
    Ok(())
}

fn handle_key_event(app: &mut App, code: KeyCode, modifiers: KeyModifiers, paths: &Paths) -> bool {
    // Ctrl+C is global quit
    if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }

    match app.view {
        View::ProcessList => handle_process_list_key(app, code, paths),
        View::LogViewer { .. } => handle_log_viewer_key(app, code, paths),
    }
}

fn handle_process_list_key(app: &mut App, code: KeyCode, paths: &Paths) -> bool {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Down | KeyCode::Char('j') => app.next(),
        KeyCode::Up | KeyCode::Char('k') => app.previous(),
        KeyCode::Home => app.first(),
        KeyCode::End => app.last(),
        KeyCode::Enter => app.open_log_viewer(paths),
        KeyCode::Char('s') => app.start_selected(paths),
        KeyCode::Char('x') => app.stop_selected(paths),
        KeyCode::Char('r') => app.restart_selected(paths),
        KeyCode::Char('S') => app.start_all(paths),
        KeyCode::Char('X') => app.stop_all(paths),
        KeyCode::Char('R') => app.restart_all(paths),
        _ => {}
    }
    false
}

fn handle_log_viewer_key(app: &mut App, code: KeyCode, paths: &Paths) -> bool {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.close_log_viewer(),
        KeyCode::Up | KeyCode::Char('k') => app.log_scroll_up(1),
        KeyCode::Down | KeyCode::Char('j') => app.log_scroll_down(1),
        KeyCode::PageUp => app.log_page_up(),
        KeyCode::PageDown => app.log_page_down(),
        KeyCode::Home | KeyCode::Char('g') => app.log_scroll_to_top(),
        KeyCode::End | KeyCode::Char('G') => app.log_scroll_to_bottom(),
        KeyCode::Tab => app.toggle_log_stream(paths),
        KeyCode::Char('s') => {
            let name = app.viewed_process_name().map(|s| s.to_string());
            if let Some(name) = name {
                app.start_named(&name, paths);
            }
        }
        KeyCode::Char('x') => {
            let name = app.viewed_process_name().map(|s| s.to_string());
            if let Some(name) = name {
                app.stop_named(&name, paths);
            }
        }
        KeyCode::Char('r') => {
            let name = app.viewed_process_name().map(|s| s.to_string());
            if let Some(name) = name {
                app.restart_named(&name, paths);
            }
        }
        _ => {}
    }
    false
}

fn ui(f: &mut Frame, app: &mut App) {
    let terminal_width = f.area().width;

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header card
            Constraint::Length(1), // spacer
            Constraint::Min(5),    // table / log viewer
            Constraint::Length(1), // spacer
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    render_header(f, app, layout[0]);

    let spacer = Paragraph::new("").style(Style::default().bg(BG_BASE));
    f.render_widget(spacer.clone(), layout[1]);
    f.render_widget(spacer, layout[3]);

    match &app.view {
        View::ProcessList => {
            render_table(f, app, layout[2], terminal_width);
            render_process_list_footer(f, app, layout[4]);
        }
        View::LogViewer { .. } => {
            render_log_viewer(f, app, layout[2]);
            render_log_viewer_footer(f, &*app, layout[4]);
        }
    }
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
        .row_highlight_style(Style::default())
        .highlight_symbol("");

    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn render_process_list_footer(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
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
        Span::styled(" ⏎ ", key_style),
        Span::styled(" logs ", label_style),
        Span::styled(" s ", key_style),
        Span::styled(" start ", label_style),
        Span::styled(" x ", key_style),
        Span::styled(" stop ", label_style),
        Span::styled(" r ", key_style),
        Span::styled(" restart ", label_style),
        Span::styled(" S ", key_style),
        Span::styled(" start all ", label_style),
        Span::styled(" X ", key_style),
        Span::styled(" stop all ", label_style),
        Span::styled(" R ", key_style),
        Span::styled(" restart all ", label_style),
    ];

    append_status_spans(&mut spans, app);

    let line = Line::from(spans);
    let footer = Paragraph::new(Text::from(line)).style(Style::default().bg(BG_BASE));
    f.render_widget(footer, area);
}

fn render_log_viewer_footer(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let key_style = Style::default()
        .fg(KEY_FG)
        .bg(KEY_BG)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(FG_DIM);

    let mut spans = vec![
        Span::styled(" ", label_style),
        Span::styled(" q ", key_style),
        Span::styled(" back ", label_style),
        Span::styled(" ↑↓ ", key_style),
        Span::styled(" scroll ", label_style),
        Span::styled(" PgUp/Dn ", key_style),
        Span::styled(" page ", label_style),
        Span::styled(" G ", key_style),
        Span::styled(" bottom ", label_style),
        Span::styled(" Tab ", key_style),
        Span::styled(" switch tab ", label_style),
        Span::styled(" s ", key_style),
        Span::styled(" start ", label_style),
        Span::styled(" x ", key_style),
        Span::styled(" stop ", label_style),
        Span::styled(" r ", key_style),
        Span::styled(" restart ", label_style),
    ];

    append_status_spans(&mut spans, app);

    let line = Line::from(spans);
    let footer = Paragraph::new(Text::from(line)).style(Style::default().bg(BG_BASE));
    f.render_widget(footer, area);
}

fn append_status_spans<'a>(spans: &mut Vec<Span<'a>>, app: &'a App) {
    let label_style = Style::default().fg(FG_DIM);

    if let Some((msg, is_success)) = app.active_status_message() {
        let (fg, bg) = if is_success {
            (FG_BRIGHT, STATUS_GREEN)
        } else {
            (FG_BRIGHT, STATUS_RED)
        };
        spans.push(Span::styled("  ", label_style));
        spans.push(Span::styled(
            format!(" {msg} "),
            Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        ));
    } else if let Some(err) = &app.last_error {
        spans.push(Span::styled("  ", label_style));
        spans.push(Span::styled(
            format!(" error: {err} "),
            Style::default()
                .fg(FG_BRIGHT)
                .bg(STATUS_RED)
                .add_modifier(Modifier::BOLD),
        ));
    }
}

fn render_log_viewer(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let (process_name, lines, scroll_offset, auto_scroll, stream) = match &app.view {
        View::LogViewer {
            process_name,
            lines,
            scroll_offset,
            auto_scroll,
            stream,
        } => (process_name, lines, *scroll_offset, *auto_scroll, stream),
        _ => return,
    };

    let stream_label = match stream {
        LogSource::Stdout => "stdout",
        LogSource::Stderr => "stderr",
    };

    // Split area: tab bar (1 line) + content
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(area);

    // ── Tab bar ─────────────────────────────────────────────────────
    let active_style = Style::default()
        .fg(FG_BRIGHT)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD);
    let inactive_style = Style::default().fg(FG_DIM).bg(BG_BASE);

    let tabs = [
        (" stdout ", LogSource::Stdout),
        (" stderr ", LogSource::Stderr),
    ];

    let mut tab_spans: Vec<Span> = vec![Span::styled(" ", Style::default().bg(BG_BASE))];

    for (label, variant) in &tabs {
        let is_active = std::mem::discriminant(stream) == std::mem::discriminant(variant);
        let style = if is_active {
            active_style
        } else {
            inactive_style
        };
        tab_spans.push(Span::styled(*label, style));
        tab_spans.push(Span::styled(" ", Style::default().bg(BG_BASE)));
    }

    let tab_line = Paragraph::new(Line::from(tab_spans)).style(Style::default().bg(BG_BASE));
    f.render_widget(tab_line, chunks[0]);

    // ── Log content ─────────────────────────────────────────────────
    let scroll_label = if auto_scroll { "following" } else { "paused" };
    let title = Line::from(vec![
        Span::styled(
            format!(" {process_name} "),
            Style::default().fg(FG_TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("({scroll_label}) "), Style::default().fg(FG_DIM)),
    ]);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT_DIM))
        .padding(Padding::horizontal(1));

    let inner = block.inner(chunks[1]);
    f.render_widget(block, chunks[1]);

    let visible_height = inner.height as usize;
    app.last_visible_height = inner.height;
    if visible_height == 0 {
        return;
    }

    if lines.is_empty() {
        let empty_msg = format!("No {stream_label} output yet");
        let text = Paragraph::new(Text::from(Line::from(Span::styled(
            empty_msg,
            Style::default().fg(FG_DIM),
        ))))
        .alignment(Alignment::Center);
        f.render_widget(text, inner);
        return;
    }

    // Calculate visible window: lines are displayed bottom-pinned
    // scroll_offset=0 means showing the last `visible_height` lines
    let total = lines.len();
    let end = total.saturating_sub(scroll_offset as usize);
    let start = end.saturating_sub(visible_height);

    let is_stderr = matches!(stream, LogSource::Stderr);
    let line_style = if is_stderr {
        Style::default().fg(STATUS_RED)
    } else {
        Style::default().fg(FG_TEXT)
    };

    let display_lines: Vec<Line> = lines[start..end]
        .iter()
        .map(|l| Line::from(Span::styled(l.as_str(), line_style)))
        .collect();

    let text = Paragraph::new(Text::from(display_lines));
    f.render_widget(text, inner);
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

fn load_configs() -> color_eyre::Result<std::collections::HashMap<String, config::ProcessConfig>> {
    let config_path = std::env::current_dir()?.join("pm3.toml");
    config::load_config(&config_path).map_err(|e| color_eyre::eyre::eyre!("{e}"))
}

const STATUS_MESSAGE_DURATION: Duration = Duration::from_secs(3);

struct App {
    processes: Vec<ProcessInfo>,
    table_state: TableState,
    last_error: Option<String>,
    status_message: Option<(String, bool, Instant)>, // (message, is_success, timestamp)
    view: View,
    last_visible_height: u16,
}

impl App {
    fn new() -> Self {
        Self {
            processes: Vec::new(),
            table_state: TableState::default(),
            last_error: None,
            status_message: None,
            view: View::ProcessList,
            last_visible_height: 20,
        }
    }

    fn set_status(&mut self, message: String, is_success: bool) {
        self.status_message = Some((message, is_success, Instant::now()));
    }

    fn active_status_message(&self) -> Option<(&str, bool)> {
        self.status_message.as_ref().and_then(|(msg, ok, when)| {
            if when.elapsed() < STATUS_MESSAGE_DURATION {
                Some((msg.as_str(), *ok))
            } else {
                None
            }
        })
    }

    fn start_selected(&mut self, paths: &Paths) {
        let name = match self.viewed_process_name() {
            Some(n) => n.to_string(),
            None => return,
        };
        self.start_named(&name, paths);
    }

    fn stop_selected(&mut self, paths: &Paths) {
        let name = match self.viewed_process_name() {
            Some(n) => n.to_string(),
            None => return,
        };
        self.stop_named(&name, paths);
    }

    fn restart_selected(&mut self, paths: &Paths) {
        let name = match self.viewed_process_name() {
            Some(n) => n.to_string(),
            None => return,
        };
        self.restart_named(&name, paths);
    }

    fn start_all(&mut self, paths: &Paths) {
        let configs = match load_configs() {
            Ok(c) => c,
            Err(e) => {
                self.set_status(format!("config error: {e}"), false);
                return;
            }
        };
        match client::send_request(
            paths,
            &Request::Start {
                configs,
                names: None,
                env: None,
            },
        ) {
            Ok(Response::Success { .. }) => {
                self.set_status("started all".to_string(), true);
                self.refresh(paths);
            }
            Ok(Response::Error { message }) => self.set_status(message, false),
            Ok(_) => self.set_status("unexpected response".to_string(), false),
            Err(e) => self.set_status(e.to_string(), false),
        }
    }

    fn stop_all(&mut self, paths: &Paths) {
        match client::send_request(paths, &Request::Stop { names: None }) {
            Ok(Response::Success { .. }) => {
                self.set_status("stopped all".to_string(), true);
                self.refresh(paths);
            }
            Ok(Response::Error { message }) => self.set_status(message, false),
            Ok(_) => self.set_status("unexpected response".to_string(), false),
            Err(e) => self.set_status(e.to_string(), false),
        }
    }

    fn restart_all(&mut self, paths: &Paths) {
        match client::send_request(paths, &Request::Restart { names: None }) {
            Ok(Response::Success { .. }) => {
                self.set_status("restarted all".to_string(), true);
                self.refresh(paths);
            }
            Ok(Response::Error { message }) => self.set_status(message, false),
            Ok(_) => self.set_status("unexpected response".to_string(), false),
            Err(e) => self.set_status(e.to_string(), false),
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

    // ── Log viewer methods ──────────────────────────────────────────

    fn viewed_process_name(&self) -> Option<&str> {
        match &self.view {
            View::LogViewer { process_name, .. } => Some(process_name.as_str()),
            View::ProcessList => self.selected_name(),
        }
    }

    fn open_log_viewer(&mut self, paths: &Paths) {
        let name = match self.selected_name() {
            Some(n) => n.to_string(),
            None => return,
        };
        self.view = View::LogViewer {
            process_name: name,
            lines: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            stream: LogSource::Stdout,
        };
        self.refresh_logs(paths);
    }

    fn close_log_viewer(&mut self) {
        self.view = View::ProcessList;
    }

    fn toggle_log_stream(&mut self, paths: &Paths) {
        if let View::LogViewer {
            stream,
            lines,
            scroll_offset,
            auto_scroll,
            ..
        } = &mut self.view
        {
            *stream = match stream {
                LogSource::Stdout => LogSource::Stderr,
                LogSource::Stderr => LogSource::Stdout,
            };
            *lines = Vec::new();
            *scroll_offset = 0;
            *auto_scroll = true;
        }
        self.refresh_logs(paths);
    }

    fn refresh_logs(&mut self, paths: &Paths) {
        if let View::LogViewer {
            process_name,
            lines,
            scroll_offset,
            auto_scroll,
            stream,
        } = &mut self.view
        {
            let path = match stream {
                LogSource::Stdout => paths.stdout_log(process_name),
                LogSource::Stderr => paths.stderr_log(process_name),
            };
            if let Ok(new_lines) = log::tail_file(&path, 200) {
                *lines = new_lines;
            }
            if *auto_scroll {
                *scroll_offset = 0;
            }
        }
    }

    fn log_scroll_up(&mut self, amount: u16) {
        if let View::LogViewer {
            lines,
            scroll_offset,
            auto_scroll,
            ..
        } = &mut self.view
        {
            *auto_scroll = false;
            let max_offset = lines.len().saturating_sub(1) as u16;
            *scroll_offset = (*scroll_offset + amount).min(max_offset);
        }
    }

    fn log_scroll_down(&mut self, amount: u16) {
        if let View::LogViewer {
            scroll_offset,
            auto_scroll,
            ..
        } = &mut self.view
        {
            if *scroll_offset <= amount {
                *scroll_offset = 0;
                *auto_scroll = true;
            } else {
                *scroll_offset -= amount;
            }
        }
    }

    fn log_page_up(&mut self) {
        let page = self.last_visible_height.max(1);
        self.log_scroll_up(page);
    }

    fn log_page_down(&mut self) {
        let page = self.last_visible_height.max(1);
        self.log_scroll_down(page);
    }

    fn log_scroll_to_top(&mut self) {
        if let View::LogViewer {
            lines,
            scroll_offset,
            auto_scroll,
            ..
        } = &mut self.view
        {
            *auto_scroll = false;
            *scroll_offset = lines.len().saturating_sub(1) as u16;
        }
    }

    fn log_scroll_to_bottom(&mut self) {
        if let View::LogViewer {
            scroll_offset,
            auto_scroll,
            ..
        } = &mut self.view
        {
            *scroll_offset = 0;
            *auto_scroll = true;
        }
    }

    fn start_named(&mut self, name: &str, paths: &Paths) {
        let configs = match load_configs() {
            Ok(c) => c,
            Err(e) => {
                self.set_status(format!("config error: {e}"), false);
                return;
            }
        };
        match client::send_request(
            paths,
            &Request::Start {
                configs,
                names: Some(vec![name.to_string()]),
                env: None,
            },
        ) {
            Ok(Response::Success { .. }) => {
                self.set_status(format!("started {name}"), true);
                self.refresh(paths);
            }
            Ok(Response::Error { message }) => self.set_status(message, false),
            Ok(_) => self.set_status("unexpected response".to_string(), false),
            Err(e) => self.set_status(e.to_string(), false),
        }
    }

    fn stop_named(&mut self, name: &str, paths: &Paths) {
        match client::send_request(
            paths,
            &Request::Stop {
                names: Some(vec![name.to_string()]),
            },
        ) {
            Ok(Response::Success { .. }) => {
                self.set_status(format!("stopped {name}"), true);
                self.refresh(paths);
            }
            Ok(Response::Error { message }) => self.set_status(message, false),
            Ok(_) => self.set_status("unexpected response".to_string(), false),
            Err(e) => self.set_status(e.to_string(), false),
        }
    }

    fn restart_named(&mut self, name: &str, paths: &Paths) {
        match client::send_request(
            paths,
            &Request::Restart {
                names: Some(vec![name.to_string()]),
            },
        ) {
            Ok(Response::Success { .. }) => {
                self.set_status(format!("restarted {name}"), true);
                self.refresh(paths);
            }
            Ok(Response::Error { message }) => self.set_status(message, false),
            Ok(_) => self.set_status("unexpected response".to_string(), false),
            Err(e) => self.set_status(e.to_string(), false),
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
