//! Ratatui dashboard: followed-live list (left) + chat (right), mpv plays video.
//!
//! CONTRACT (do not change this public signature):
//!   - `run(client, auth)` runs the full TUI until the user quits.
//!
//! Layout (responsive): left ~40% followed-live list, right ~60% chat; a 1-line
//! status bar at the bottom plus, in Chat mode, a 1-line input box. Async event
//! loop multiplexes keyboard, incoming chat, a 60s list refresh and a 250ms
//! render tick via `tokio::select!`.

use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::auth::Auth;
use crate::chat::{self, ChatEvent, ChatHandle, ChatMessage};
use crate::helix::{self, humanize_count, uptime, Stream};
use crate::playback::PlaybackSession;

const CHAT_CAP: usize = 500;
const REFRESH_SECS: u64 = 60;
const RENDER_MS: u64 = 250;
/// Fallback username color when a message carries no `color` tag.
const DEFAULT_USER_COLOR: Color = Color::Rgb(0x8a, 0x8a, 0xff);

type Backend = CrosstermBackend<Stdout>;

/// Which pane currently has focus / receives keystrokes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    Chat,
}

/// How the followed-live list is ordered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Viewers,
    Name,
    Uptime,
}

impl Sort {
    fn next(self) -> Sort {
        match self {
            Sort::Viewers => Sort::Name,
            Sort::Name => Sort::Uptime,
            Sort::Uptime => Sort::Viewers,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Sort::Viewers => "viewers",
            Sort::Name => "name",
            Sort::Uptime => "uptime",
        }
    }
}

/// Restores the terminal (raw mode + alternate screen) when dropped, so normal
/// exits and `?`-propagated errors both leave the shell clean.
struct TerminalGuard {
    terminal: Terminal<Backend>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = restore_terminal();
                Err(error.into())
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        let _ = restore_terminal();
    }
}

/// Low-level terminal restore, also used by the panic hook.
fn restore_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    let screen_result = execute!(stdout, LeaveAlternateScreen);
    let raw_result = disable_raw_mode();
    screen_result.and(raw_result)
}

/// All mutable UI state. Deliberately separated from rendering so transitions can
/// be reasoned about (and unit-tested) without a real terminal.
struct App {
    mode: Mode,
    sort: Sort,
    streams: Vec<Stream>,
    list_state: ListState,
    chat: VecDeque<ChatMessage>,
    chat_scroll: u16,
    /// The channel chat is currently joined to / playing, if any.
    current_channel: Option<String>,
    input: String,
    status: String,
    play_state: String,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            mode: Mode::List,
            sort: Sort::Viewers,
            streams: Vec::new(),
            list_state: ListState::default(),
            chat: VecDeque::with_capacity(CHAT_CAP),
            chat_scroll: 0,
            current_channel: None,
            input: String::new(),
            status: "starting…".to_string(),
            play_state: "idle".to_string(),
            should_quit: false,
        }
    }

    fn selected(&self) -> Option<&Stream> {
        self.list_state.selected().and_then(|i| self.streams.get(i))
    }

    fn move_selection(&mut self, delta: isize) {
        if self.streams.is_empty() {
            self.list_state.select(None);
            return;
        }
        let len = self.streams.len() as isize;
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(len);
        self.list_state.select(Some(next as usize));
    }

    /// Re-sort in place, preserving the selected channel by `user_login`.
    fn apply_sort(&mut self) {
        let keep = self.selected().map(|s| s.user_login.clone());
        sort_streams(&mut self.streams, self.sort);
        self.restore_selection(keep);
    }

    /// After replacing/sorting the list, keep the cursor on the same channel if
    /// it's still present; otherwise clamp to a valid index.
    fn restore_selection(&mut self, keep: Option<String>) {
        if self.streams.is_empty() {
            self.list_state.select(None);
            return;
        }
        let idx = keep
            .and_then(|login| self.streams.iter().position(|s| s.user_login == login))
            .unwrap_or(0)
            .min(self.streams.len() - 1);
        self.list_state.select(Some(idx));
    }

    fn push_chat(&mut self, msg: ChatMessage) {
        if self.chat.len() >= CHAT_CAP {
            self.chat.pop_front();
        }
        self.chat.push_back(msg);
    }

    fn push_system(&mut self, text: impl Into<String>) {
        self.push_chat(ChatMessage {
            user: "*".to_string(),
            color: Some("#888888".to_string()),
            text: text.into(),
        });
    }
}

struct Resources {
    client: reqwest::Client,
    auth: Auth,
    chat_handle: Option<ChatHandle>,
    chat_rx: Option<UnboundedReceiver<ChatEvent>>,
    playback: Option<PlaybackSession>,
    engine_tx: UnboundedSender<String>,
}

/// Run the full TUI dashboard. Sets up the terminal (raw mode + alternate screen),
/// runs the async event loop, and restores the terminal on exit (and on panic).
pub async fn run(client: reqwest::Client, auth: Auth) -> Result<()> {
    let mut guard = TerminalGuard::new()?;
    let result = event_loop(&mut guard.terminal, client, auth).await;
    // `guard` Drop restores the terminal here on both Ok and Err paths.
    drop(guard);
    result
}

async fn event_loop(
    terminal: &mut Terminal<Backend>,
    client: reqwest::Client,
    auth: Auth,
) -> Result<()> {
    let mut app = App::new();
    let (engine_tx, mut engine_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut resources = Resources {
        client,
        auth,
        chat_handle: None,
        chat_rx: None,
        playback: None,
        engine_tx,
    };

    refresh_streams(&mut app, &mut resources).await;

    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_secs(REFRESH_SECS));
    refresh.tick().await; // consume the immediate first tick.
    let mut render = tokio::time::interval(Duration::from_millis(RENDER_MS));

    draw(terminal, &mut app)?;

    loop {
        // `chat_rx` may be None; recv() on a None receiver must never resolve, so
        // we branch the select arm on its presence.
        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        handle_key(key, &mut app, &mut resources).await;
                    }
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Err(_)) | None => { app.should_quit = true; }
                    _ => {}
                }
            }
            ev = recv_chat(&mut resources.chat_rx), if resources.chat_rx.is_some() => {
                match ev {
                    Some(event) => apply_chat_event(&mut app, event),
                    None => { resources.chat_rx = None; }
                }
            }
            _ = refresh.tick() => {
                refresh_streams(&mut app, &mut resources).await;
            }
            Some(msg) = engine_rx.recv() => {
                app.play_state = msg;
                app.status = list_status(&app);
            }
            _ = render.tick() => {}
        }

        if app.should_quit {
            break;
        }
        draw(terminal, &mut app)?;
    }

    // Tear down external resources before the terminal guard restores the screen.
    if let Some(session) = resources.playback.take() {
        session.stop().await;
    }
    Ok(())
}

async fn refresh_streams(app: &mut App, resources: &mut Resources) {
    match helix::followed_live(&resources.client, &mut resources.auth).await {
        Ok(mut streams) => {
            let selected = app.selected().map(|stream| stream.user_login.clone());
            sort_streams(&mut streams, app.sort);
            app.streams = streams;
            app.restore_selection(selected);
            app.status = list_status(app);
        }
        Err(error) => app.status = format!("refresh error: {error}"),
    }
}

/// Helper so the `select!` arm has a concrete future even when the receiver is
/// absent (guarded by the `if` precondition on the arm).
async fn recv_chat(rx: &mut Option<UnboundedReceiver<ChatEvent>>) -> Option<ChatEvent> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

fn apply_chat_event(app: &mut App, event: ChatEvent) {
    match event {
        ChatEvent::Connected => app.push_system("connected"),
        ChatEvent::Message(m) => app.push_chat(m),
        ChatEvent::System(s) => app.push_system(s),
        ChatEvent::Cleared => {
            app.chat.clear();
            app.push_system("chat cleared");
        }
        ChatEvent::Reconnecting => app.push_system("reconnecting…"),
    }
}

async fn handle_key(key: KeyEvent, app: &mut App, resources: &mut Resources) {
    // Ignore key-release events (Windows / some terminals emit them).
    if key.kind == KeyEventKind::Release {
        return;
    }

    // Ctrl-C quits from any mode.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }

    match app.mode {
        Mode::List => match key.code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Char('s') => {
                app.sort = app.sort.next();
                app.apply_sort();
                app.status = list_status(app);
            }
            KeyCode::Char('r') => refresh_streams(app, resources).await,
            KeyCode::Char('c') | KeyCode::Tab => app.mode = Mode::Chat,
            KeyCode::Enter => {
                watch_selected(app, resources).await;
            }
            _ => {}
        },
        Mode::Chat => match key.code {
            KeyCode::Esc | KeyCode::Tab => app.mode = Mode::List,
            KeyCode::Enter => {
                let text = app.input.trim().to_string();
                if !text.is_empty() {
                    if let Some(handle) = resources.chat_handle.as_ref() {
                        handle.send(text);
                    } else {
                        app.push_system("no chat connection — press Enter on a channel first");
                    }
                }
                app.input.clear();
            }
            KeyCode::Backspace => {
                app.input.pop();
            }
            KeyCode::PageUp => {
                app.chat_scroll = app.chat_scroll.saturating_add(5);
            }
            KeyCode::PageDown => {
                app.chat_scroll = app.chat_scroll.saturating_sub(5);
            }
            KeyCode::Char(c) => app.input.push(c),
            _ => {}
        },
    }
}

/// Enter (watch): stop any existing playback, start a fresh session for the
/// selected channel, then connect-or-switch the single chat connection.
async fn watch_selected(app: &mut App, resources: &mut Resources) {
    let channel = match app.selected() {
        Some(s) => s.user_login.clone(),
        None => {
            app.status = "nothing selected".to_string();
            return;
        }
    };

    // Stop the old session (kills old mpv + server) before starting a new one.
    if let Some(session) = resources.playback.take() {
        session.stop().await;
    }

    app.play_state = "starting…".to_string();
    match PlaybackSession::start(
        &resources.client,
        &channel,
        "best",
        "mpv",
        Some(resources.engine_tx.clone()),
    )
    .await
    {
        Ok(session) => {
            resources.playback = Some(session);
            app.play_state = "playing".to_string();
        }
        Err(e) => {
            app.play_state = format!("play error: {e}");
        }
    }

    // Single chat connection for the whole app: connect once, switch thereafter.
    match resources.chat_handle.as_ref() {
        Some(handle) => {
            handle.switch(channel.clone());
            app.chat.clear();
            app.chat_scroll = 0;
            app.push_system(format!("switched to {channel}"));
        }
        None => match chat::connect(&channel, &resources.auth).await {
            Ok((handle, rx)) => {
                resources.chat_handle = Some(handle);
                resources.chat_rx = Some(rx);
                app.chat.clear();
                app.chat_scroll = 0;
            }
            Err(e) => app.push_system(format!("chat connect failed: {e}")),
        },
    }

    app.current_channel = Some(channel);
    app.status = list_status(app);
}

fn list_status(app: &App) -> String {
    let ch = app.current_channel.as_deref().unwrap_or("—");
    format!(
        "{ch} ▸ {} · sort:{} · {} live",
        app.play_state,
        app.sort.label(),
        app.streams.len()
    )
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn draw(terminal: &mut Terminal<Backend>, app: &mut App) -> Result<()> {
    terminal.draw(|f| ui(f, app))?;
    Ok(())
}

fn ui(f: &mut Frame, app: &mut App) {
    let chat_mode = app.mode == Mode::Chat;
    // Bottom rows: 1-line status bar always, plus a 1-line input box in Chat mode.
    let bottom = if chat_mode { 2 } else { 1 };
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(bottom)])
        .split(f.area());

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(outer[0]);

    render_list(f, app, panes[0]);
    render_chat(f, app, panes[1]);
    render_bottom(f, app, outer[1], chat_mode);
}

fn render_list(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.mode == Mode::List;
    let title = format!("Followed · LIVE ({})", app.streams.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(focused));

    if app.streams.is_empty() {
        let placeholder = Paragraph::new("nobody you follow is live — press r to refresh")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        f.render_widget(placeholder, area);
        return;
    }

    let items: Vec<ListItem> = app
        .streams
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let selected = app.list_state.selected() == Some(i);
            let bullet = if selected { "●" } else { " " };
            let line = format!(
                "{bullet} {:<16} {:<16} {:>7} {:>6}",
                truncate(&s.user_name, 16),
                truncate(&s.game_name, 16),
                humanize_count(s.viewer_count),
                uptime(&s.started_at),
            );
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_chat(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.mode == Mode::Chat;
    let channel = app.current_channel.as_deref().unwrap_or("none");
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("chat: {channel}"))
        .border_style(border_style(focused));

    let lines: Vec<Line> = app
        .chat
        .iter()
        .map(|m| {
            let color = parse_hex_color(m.color.as_deref()).unwrap_or(DEFAULT_USER_COLOR);
            Line::from(vec![
                Span::styled(
                    m.user.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(": "),
                Span::raw(m.text.clone()),
            ])
        })
        .collect();

    // Anchor at the bottom; PgUp/PgDn nudges `chat_scroll` lines back up.
    let inner_h = area.height.saturating_sub(2); // borders
    let total = lines.len() as u16;
    let max_off = total.saturating_sub(inner_h);
    let scroll = max_off.saturating_sub(app.chat_scroll.min(max_off));

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);
}

fn render_bottom(f: &mut Frame, app: &App, area: Rect, chat_mode: bool) {
    if chat_mode {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);
        render_status(f, app, rows[0]);
        let input =
            Paragraph::new(format!("> {}", app.input)).style(Style::default().fg(Color::White));
        f.render_widget(input, rows[1]);
        // Place the cursor at the end of the input line.
        let cx = rows[1].x + 2 + app.input.chars().count() as u16;
        f.set_cursor_position((
            cx.min(rows[1].x + rows[1].width.saturating_sub(1)),
            rows[1].y,
        ));
    } else {
        render_status(f, app, area);
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let keys = match app.mode {
        Mode::List => "↑↓/jk move · ⏎ watch · s sort · r refresh · c/Tab chat · q quit",
        Mode::Chat => "type · ⏎ send · Esc/Tab list · PgUp/PgDn scroll · Ctrl-C quit",
    };
    let text = Line::from(vec![
        Span::styled(
            format!(" {} ", app.status),
            Style::default().bg(Color::Blue).fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled(keys, Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(text), area);
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------

/// Parse a `#RRGGBB` hex string into a `ratatui` RGB color. Returns None for
/// missing/empty/malformed input so callers can fall back to a default.
fn parse_hex_color(hex: Option<&str>) -> Option<Color> {
    let (r, g, b) = parse_hex_rgb(hex?)?;
    Some(Color::Rgb(r, g, b))
}

/// Parse `#RRGGBB` (case-insensitive, leading `#` required) into (r, g, b).
fn parse_hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let s = hex.strip_prefix('#')?;
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Sort the in-memory stream list per the chosen ordering.
fn sort_streams(streams: &mut [Stream], sort: Sort) {
    match sort {
        Sort::Viewers => streams.sort_by(|a, b| b.viewer_count.cmp(&a.viewer_count)),
        Sort::Name => {
            streams.sort_by(|a, b| a.user_name.to_lowercase().cmp(&b.user_name.to_lowercase()))
        }
        // Earlier `started_at` == longer uptime. RFC3339 sorts lexicographically
        // for a fixed offset; ascending string order ⇒ oldest (longest) first.
        Sort::Uptime => streams.sort_by(|a, b| a.started_at.cmp(&b.started_at)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(login: &str, name: &str, viewers: u64, started: &str) -> Stream {
        Stream {
            user_login: login.to_string(),
            user_name: name.to_string(),
            game_name: "Game".to_string(),
            title: "Title".to_string(),
            thumbnail_url: String::new(),
            viewer_count: viewers,
            started_at: started.to_string(),
        }
    }

    #[test]
    fn parse_hex_rgb_ok() {
        assert_eq!(parse_hex_rgb("#FF7F50"), Some((0xFF, 0x7F, 0x50)));
        assert_eq!(parse_hex_rgb("#000000"), Some((0, 0, 0)));
        assert_eq!(parse_hex_rgb("#ffffff"), Some((255, 255, 255)));
        assert_eq!(parse_hex_rgb("#abcdef"), Some((0xab, 0xcd, 0xef)));
    }

    #[test]
    fn parse_hex_rgb_rejects_bad() {
        assert_eq!(parse_hex_rgb("FF7F50"), None); // no leading #
        assert_eq!(parse_hex_rgb("#FFF"), None); // too short
        assert_eq!(parse_hex_rgb("#GGGGGG"), None); // non-hex
        assert_eq!(parse_hex_rgb("#FF7F500"), None); // too long
        assert_eq!(parse_hex_rgb(""), None);
    }

    #[test]
    fn parse_hex_color_handles_option() {
        assert_eq!(parse_hex_color(None), None);
        assert_eq!(
            parse_hex_color(Some("#102030")),
            Some(Color::Rgb(16, 32, 48))
        );
        assert_eq!(parse_hex_color(Some("bogus")), None);
    }

    #[test]
    fn sort_by_viewers_desc() {
        let mut v = vec![
            stream("a", "A", 10, "2026-06-24T10:00:00Z"),
            stream("b", "B", 30, "2026-06-24T10:00:00Z"),
            stream("c", "C", 20, "2026-06-24T10:00:00Z"),
        ];
        sort_streams(&mut v, Sort::Viewers);
        let order: Vec<_> = v.iter().map(|s| s.user_login.as_str()).collect();
        assert_eq!(order, vec!["b", "c", "a"]);
    }

    #[test]
    fn sort_by_name_case_insensitive() {
        let mut v = vec![
            stream("z", "zeta", 1, "2026-06-24T10:00:00Z"),
            stream("a", "Alpha", 1, "2026-06-24T10:00:00Z"),
            stream("b", "beta", 1, "2026-06-24T10:00:00Z"),
        ];
        sort_streams(&mut v, Sort::Name);
        let order: Vec<_> = v.iter().map(|s| s.user_login.as_str()).collect();
        assert_eq!(order, vec!["a", "b", "z"]);
    }

    #[test]
    fn sort_by_uptime_oldest_first() {
        let mut v = vec![
            stream("new", "New", 1, "2026-06-24T12:00:00Z"),
            stream("old", "Old", 1, "2026-06-24T08:00:00Z"),
            stream("mid", "Mid", 1, "2026-06-24T10:00:00Z"),
        ];
        sort_streams(&mut v, Sort::Uptime);
        let order: Vec<_> = v.iter().map(|s| s.user_login.as_str()).collect();
        assert_eq!(order, vec!["old", "mid", "new"]);
    }

    #[test]
    fn selection_preserved_across_resort() {
        let mut app = App::new();
        app.streams = vec![
            stream("a", "A", 10, "2026-06-24T10:00:00Z"),
            stream("b", "B", 30, "2026-06-24T10:00:00Z"),
            stream("c", "C", 20, "2026-06-24T10:00:00Z"),
        ];
        app.list_state.select(Some(0)); // "a"
        app.sort = Sort::Viewers;
        app.apply_sort();
        // "a" moved to the end under viewers-desc; cursor should follow it.
        assert_eq!(app.selected().map(|s| s.user_login.as_str()), Some("a"));
    }

    #[test]
    fn chat_buffer_caps() {
        let mut app = App::new();
        for i in 0..(CHAT_CAP + 50) {
            app.push_system(format!("m{i}"));
        }
        assert_eq!(app.chat.len(), CHAT_CAP);
        // Oldest entries were dropped from the front.
        assert_eq!(app.chat.front().unwrap().text, "m50");
    }
}
