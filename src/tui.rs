use anyhow::{Context, Result, anyhow};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    io::{self, Stdout},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    capture,
    config::StreamProfile,
    network::{self, RemoteAccessMode, UrlSet},
    state::AppState,
};

const NETWORK_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const TOAST_TTL: Duration = Duration::from_secs(4);
const ACTIONS: [ActionId; 12] = [
    ActionId::RefreshNetwork,
    ActionId::RefreshDisplays,
    ActionId::TailscaleUrl,
    ActionId::Monitor,
    ActionId::StreamProfile,
    ActionId::TogglePointer,
    ActionId::ToggleKeyboard,
    ActionId::GeneratePairingCode,
    ActionId::DisconnectSession,
    ActionId::ForgetTrustedBrowsers,
    ActionId::PanicStop,
    ActionId::Quit,
];

pub fn run(state: Arc<AppState>) -> Result<()> {
    let mut terminal = HostTerminal::enter()?;
    let mut app = HostTui::new(state);

    loop {
        app.on_tick();
        terminal.draw(|frame| app.render(frame))?;

        if !event::poll(Duration::from_millis(250)).context("failed to poll for terminal input")? {
            continue;
        }

        let Event::Key(key) = event::read().context("failed to read a terminal event")? else {
            continue;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }

        if app.handle_key(key.code)? {
            break;
        }
    }

    Ok(())
}

struct HostTerminal {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl HostTerminal {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw terminal mode")?;

        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, cursor::Hide)
            .context("failed to switch to the alternate terminal screen")?;

        let backend = CrosstermBackend::new(stdout);
        let mut terminal =
            Terminal::new(backend).context("failed to initialize the terminal backend")?;
        terminal.clear().context("failed to clear the terminal")?;

        Ok(Self { terminal })
    }

    fn draw<F>(&mut self, draw_fn: F) -> Result<()>
    where
        F: FnOnce(&mut Frame),
    {
        self.terminal
            .draw(draw_fn)
            .context("failed to draw a terminal frame")?;
        Ok(())
    }
}

impl Drop for HostTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            cursor::Show
        );
        let _ = self.terminal.show_cursor();
    }
}

struct Toast {
    message: String,
    created_at: Instant,
}

struct HostTui {
    state: Arc<AppState>,
    urls: UrlSet,
    selected_action: usize,
    last_network_refresh: Instant,
    toast: Option<Toast>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionId {
    RefreshNetwork,
    RefreshDisplays,
    TailscaleUrl,
    Monitor,
    StreamProfile,
    TogglePointer,
    ToggleKeyboard,
    GeneratePairingCode,
    DisconnectSession,
    ForgetTrustedBrowsers,
    PanicStop,
    Quit,
}

impl HostTui {
    fn new(state: Arc<AppState>) -> Self {
        Self {
            urls: network::discover_urls(state.port()),
            state,
            selected_action: 0,
            last_network_refresh: Instant::now(),
            toast: None,
        }
    }

    fn on_tick(&mut self) {
        if self.last_network_refresh.elapsed() >= NETWORK_REFRESH_INTERVAL {
            self.refresh_network(false);
        }

        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.created_at.elapsed() >= TOAST_TTL)
        {
            self.toast = None;
        }
    }

    fn handle_key(&mut self, key: KeyCode) -> Result<bool> {
        match key {
            KeyCode::Up => self.shift_selection(-1),
            KeyCode::Down => self.shift_selection(1),
            KeyCode::Left => self.adjust_selected(-1)?,
            KeyCode::Right => self.adjust_selected(1)?,
            KeyCode::Enter => {
                if self.activate_selected()? {
                    return Ok(true);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => return Ok(true),
            _ => {}
        }

        Ok(false)
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(0),
                Constraint::Length(3),
            ])
            .split(area);

        self.render_header(frame, layout[0]);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(44), Constraint::Min(0)])
            .split(layout[1]);
        self.render_actions(frame, body[0]);
        self.render_status_panels(frame, body[1]);

        self.render_footer(frame, layout[2]);
    }

    fn render_header(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let toast_line = self.toast.as_ref().map_or_else(
            || Line::from(self.remote_access_summary()),
            |toast| {
                Line::from(vec![
                    Span::styled("Notice: ", Style::default().fg(Color::Yellow)),
                    Span::raw(&toast.message),
                ])
            },
        );

        let header = Paragraph::new(Text::from(vec![
            Line::from(vec![
                Span::styled(
                    "RustOp Viewer",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  browser client + host TUI"),
            ]),
            toast_line,
        ]))
        .block(Block::default().borders(Borders::ALL).title("Host"))
        .wrap(Wrap { trim: true });

        frame.render_widget(header, area);
    }

    fn render_actions(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let items = ACTIONS
            .iter()
            .map(|action| ListItem::new(Line::from(self.action_label(*action))))
            .collect::<Vec<_>>();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Actions"))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› ");

        let mut state = ListState::default();
        state.select(Some(self.selected_action));
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_status_panels(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let panels = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Min(0),
            ])
            .split(area);

        frame.render_widget(self.overview_panel(), panels[0]);
        frame.render_widget(self.access_panel(), panels[1]);
        frame.render_widget(self.security_panel(), panels[2]);
    }

    fn render_footer(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let footer = Paragraph::new(Text::from(vec![
            Line::from("Up/Down select  Left/Right adjust  Enter activate  q quit"),
            Line::from(self.action_help(self.selected_action())),
        ]))
        .block(Block::default().borders(Borders::ALL).title("Keys"))
        .wrap(Wrap { trim: true });

        frame.render_widget(footer, area);
    }

    fn overview_panel(&self) -> Paragraph<'static> {
        let monitor_summary = self
            .state
            .selected_monitor()
            .map(|monitor| {
                format!(
                    "{} • {} @ {},{}",
                    monitor.display_name(),
                    monitor.resolution_label(),
                    monitor.x,
                    monitor.y
                )
            })
            .unwrap_or_else(|| "No display detected".to_string());

        let frame_summary = self.state.latest_frame().map_or_else(
            || "Waiting for the first frame capture".to_string(),
            |frame| {
                let age_ms = frame
                    .captured_at
                    .elapsed()
                    .map(|elapsed| elapsed.as_millis())
                    .unwrap_or(0);
                format!(
                    "{}x{} from {}x{}  {}  {} ms old",
                    frame.encoded_width,
                    frame.encoded_height,
                    frame.source_width,
                    frame.source_height,
                    format_bytes_compact(frame.byte_len as u64),
                    age_ms
                )
            },
        );

        let capture_status = self
            .state
            .capture_error()
            .map(|error| format!("Capture issue: {error}"))
            .unwrap_or_else(|| "Capture loop healthy".to_string());

        Paragraph::new(Text::from(vec![
            Line::from(format!(
                "Port: {}   Elevated: {}",
                self.state.port(),
                yes_no(self.state.is_elevated())
            )),
            Line::from(format!("Monitor: {monitor_summary}")),
            Line::from(format!(
                "Stream: {}  {}",
                self.state.stream_profile().label(),
                self.state.stream_profile().summary()
            )),
            Line::from(format!("Latest frame: {frame_summary}")),
            Line::from(capture_status),
            Line::from(format!("Config: {}", self.state.config_path().display())),
        ]))
        .block(Block::default().borders(Borders::ALL).title("Overview"))
        .wrap(Wrap { trim: true })
    }

    fn access_panel(&self) -> Paragraph<'static> {
        let mut lines = vec![
            Line::from(format!(
                "Mode: {}",
                remote_access_mode_label(self.urls.tailscale_status.remote_access_mode())
            )),
            Line::from(format!(
                "Preferred ({}): {}",
                self.urls.preferred.label, self.urls.preferred.url
            )),
            Line::from(format!(
                "Loopback ({}): {}",
                self.urls.loopback.label, self.urls.loopback.url
            )),
        ];

        if let Some(url) = &self.urls.mobile_data_preferred {
            lines.push(Line::from(format!(
                "Mobile data path ({}): {}",
                url.label, url.url
            )));
        }

        let discovered_paths = [
            self.urls
                .tailscale_http
                .as_ref()
                .map(|url| format!("{}: {}", url.label, url.url)),
            self.urls
                .tailscale_serve_http
                .as_ref()
                .map(|url| format!("{}: {}", url.label, url.url)),
            self.urls
                .tailscale_https
                .as_ref()
                .map(|url| format!("{}: {}", url.label, url.url)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

        if !discovered_paths.is_empty() {
            lines.push(Line::from(format!(
                "Private paths: {}",
                discovered_paths.join("  |  ")
            )));
        }

        let host_tailnet = match (
            self.urls.tailscale_status.host_name.as_deref(),
            self.urls.tailscale_status.tailnet_name.as_deref(),
        ) {
            (Some(host_name), Some(tailnet_name)) => {
                format!("Host: {host_name}   Tailnet: {tailnet_name}")
            }
            (Some(host_name), None) => format!("Host: {host_name}"),
            (None, Some(tailnet_name)) => format!("Tailnet: {tailnet_name}"),
            (None, None) => "Host: unavailable".to_string(),
        };
        lines.push(Line::from(host_tailnet));

        lines.push(Line::from(format!(
            "MagicDNS: {}   Tailscale HTTPS certs: {}",
            yes_no(self.urls.tailscale_status.magic_dns_enabled),
            yes_no(self.urls.tailscale_status.https_certificates_available)
        )));

        Paragraph::new(Text::from(lines))
            .block(Block::default().borders(Borders::ALL).title("Access"))
            .wrap(Wrap { trim: true })
    }

    fn security_panel(&self) -> Paragraph<'static> {
        let pointer_state = if self.state.remote_pointer_requested() {
            "enabled"
        } else {
            "disabled"
        };
        let keyboard_state = if self.state.remote_keyboard_requested() {
            "enabled"
        } else {
            "disabled"
        };

        let mut lines = vec![
            Line::from(format!("Pointer scope: {pointer_state}")),
            Line::from(format!("Keyboard scope: {keyboard_state}")),
        ];

        if let Some(code) = self.state.current_pair_code() {
            lines.push(Line::from(format!(
                "Pair code: {}   expires in {}   attempts left {}",
                code.code,
                format_duration_compact(code.expires_in),
                code.remaining_attempts
            )));
        } else {
            lines.push(Line::from("Pair code: none active"));
        }

        if let Some(session) = self.state.current_remote_session() {
            lines.push(Line::from(format!(
                "Session: expires in {}   data sent {}",
                format_duration_compact(session.expires_in),
                format_bytes_compact(session.bytes_sent)
            )));
            lines.push(Line::from(format!(
                "Frames {}   cached {}   status {}",
                session.frame_responses, session.cached_frame_hits, session.status_responses
            )));
            if let Some(user_agent) = self.state.current_remote_user_agent() {
                lines.push(Line::from(format!("User-Agent: {user_agent}")));
            }
        } else {
            lines.push(Line::from("Session: no approved remote browser"));
        }

        let trusted_browsers = self.state.trusted_browser_snapshots();
        if trusted_browsers.is_empty() {
            lines.push(Line::from("Trusted browsers: none remembered"));
        } else {
            lines.push(Line::from(format!(
                "Trusted browsers: {} remembered",
                trusted_browsers.len()
            )));
            for browser in trusted_browsers.iter().take(3) {
                lines.push(Line::from(format!(
                    "{} [{}]   seen {} ago   added {} ago",
                    browser.label,
                    browser.id,
                    format_duration_compact(browser.last_seen_ago),
                    format_duration_compact(browser.created_ago)
                )));
            }
            if trusted_browsers.len() > 3 {
                lines.push(Line::from(format!(
                    "... plus {} more remembered browser(s)",
                    trusted_browsers.len() - 3
                )));
            }
        }

        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Pairing & Session"),
            )
            .wrap(Wrap { trim: true })
    }

    fn selected_action(&self) -> ActionId {
        ACTIONS[self.selected_action]
    }

    fn shift_selection(&mut self, delta: isize) {
        let len = ACTIONS.len() as isize;
        self.selected_action = ((self.selected_action as isize + delta).rem_euclid(len)) as usize;
    }

    fn activate_selected(&mut self) -> Result<bool> {
        match self.selected_action() {
            ActionId::RefreshNetwork => {
                self.refresh_network(true);
            }
            ActionId::RefreshDisplays => {
                self.refresh_displays()?;
            }
            ActionId::TailscaleUrl => {
                self.enable_tailscale_url();
            }
            ActionId::Monitor => {
                self.cycle_monitor(1)?;
            }
            ActionId::StreamProfile => {
                self.cycle_stream_profile(1)?;
            }
            ActionId::TogglePointer => {
                self.toggle_pointer();
            }
            ActionId::ToggleKeyboard => {
                self.toggle_keyboard();
            }
            ActionId::GeneratePairingCode => {
                let snapshot = self.state.generate_pair_code();
                self.set_toast(format!("Pairing code {} is ready", snapshot.code));
            }
            ActionId::DisconnectSession => {
                if self.state.current_remote_session().is_some() {
                    self.state.revoke_remote_session();
                    self.set_toast("Disconnected the approved remote session");
                } else {
                    self.set_toast("No remote session is currently active");
                }
            }
            ActionId::ForgetTrustedBrowsers => match self.state.revoke_trusted_browsers() {
                Ok(0) => self.set_toast("No trusted browsers were remembered"),
                Ok(count) => {
                    self.set_toast(format!("Forgot {count} trusted browser(s) and cleared the current session"))
                }
                Err(err) => self.set_toast(format!("Trusted browser cleanup failed: {err}")),
            },
            ActionId::PanicStop => match self.state.panic_stop() {
                Ok(()) => {
                    self.set_toast("Remote input disabled and every pairing, session, and trusted browser was cleared")
                }
                Err(err) => self.set_toast(format!("Panic stop failed: {err}")),
            },
            ActionId::Quit => return Ok(true),
        }

        Ok(false)
    }

    fn adjust_selected(&mut self, delta: isize) -> Result<()> {
        match self.selected_action() {
            ActionId::Monitor => self.cycle_monitor(delta)?,
            ActionId::StreamProfile => self.cycle_stream_profile(delta)?,
            _ => {}
        }

        Ok(())
    }

    fn refresh_network(&mut self, announce: bool) {
        self.urls = network::discover_urls(self.state.port());
        self.last_network_refresh = Instant::now();
        if announce {
            self.set_toast("Refreshed access URL discovery");
        }
    }

    fn refresh_displays(&mut self) -> Result<()> {
        let monitors =
            capture::discover_monitors().context("failed to refresh display monitors")?;
        let monitor_count = monitors.len();
        self.state.set_monitors(monitors);
        self.state
            .ensure_valid_selected_monitor()
            .context("failed to keep a valid monitor selection")?;
        self.set_toast(format!("Refreshed {monitor_count} display(s)"));
        Ok(())
    }

    fn enable_tailscale_url(&mut self) {
        if !self.urls.tailscale_status.is_running {
            self.set_toast("Tailscale is not running on this host");
            return;
        }

        if self.urls.tailscale_status.serve_enabled {
            self.refresh_network(false);
            self.set_toast("Refreshed the Tailscale URL");
            return;
        }

        match network::enable_tailscale_client_url(self.state.port()) {
            Ok(()) => {
                self.refresh_network(false);
                self.set_toast("Enabled a Tailscale URL for this host");
            }
            Err(err) => self.set_toast(format!("Tailscale URL setup failed: {err}")),
        }
    }

    fn cycle_monitor(&mut self, delta: isize) -> Result<()> {
        let monitors = self.state.monitors();
        if monitors.is_empty() {
            return Err(anyhow!("no display monitors are available"));
        }

        let current_index = self
            .state
            .selected_monitor_id()
            .and_then(|selected_id| {
                monitors
                    .iter()
                    .position(|monitor| monitor.id == selected_id)
            })
            .unwrap_or(0);
        let next_index =
            ((current_index as isize + delta).rem_euclid(monitors.len() as isize)) as usize;
        let next_monitor = &monitors[next_index];
        self.state
            .set_selected_monitor(next_monitor.id)
            .context("failed to change the selected monitor")?;
        self.set_toast(format!("Selected monitor: {}", next_monitor.display_name()));
        Ok(())
    }

    fn cycle_stream_profile(&mut self, delta: isize) -> Result<()> {
        let profiles = [
            StreamProfile::Balanced,
            StreamProfile::DataSaver,
            StreamProfile::Emergency,
        ];
        let current_index = profiles
            .iter()
            .position(|profile| *profile == self.state.stream_profile())
            .unwrap_or(0);
        let next_index =
            ((current_index as isize + delta).rem_euclid(profiles.len() as isize)) as usize;
        let next_profile = profiles[next_index];
        self.state
            .set_stream_profile(next_profile)
            .context("failed to change the stream profile")?;
        self.set_toast(format!("Stream profile: {}", next_profile.label()));
        Ok(())
    }

    fn toggle_pointer(&mut self) {
        let next_enabled = !self.state.remote_pointer_requested();
        match self.state.set_remote_pointer_enabled(next_enabled) {
            Ok(()) => self.set_toast(format!(
                "Remote pointer {}",
                if next_enabled { "enabled" } else { "disabled" }
            )),
            Err(err) => self.set_toast(format!("Pointer control update failed: {err}")),
        }
    }

    fn toggle_keyboard(&mut self) {
        let next_enabled = !self.state.remote_keyboard_requested();
        match self.state.set_remote_keyboard_enabled(next_enabled) {
            Ok(()) => self.set_toast(format!(
                "Remote keyboard {}",
                if next_enabled { "enabled" } else { "disabled" }
            )),
            Err(err) => self.set_toast(format!("Keyboard control update failed: {err}")),
        }
    }

    fn action_label(&self, action: ActionId) -> String {
        match action {
            ActionId::RefreshNetwork => "Refresh access URLs".to_string(),
            ActionId::RefreshDisplays => {
                format!("Refresh displays ({})", self.state.monitors().len())
            }
            ActionId::TailscaleUrl => {
                if !self.urls.tailscale_status.is_running {
                    "Enable Tailscale URL (offline)".to_string()
                } else if self.urls.tailscale_status.serve_enabled {
                    "Refresh Tailscale URL".to_string()
                } else {
                    "Enable Tailscale URL".to_string()
                }
            }
            ActionId::Monitor => format!(
                "Monitor: {}",
                self.state
                    .selected_monitor()
                    .map(|monitor| monitor.display_name())
                    .unwrap_or_else(|| "none".to_string())
            ),
            ActionId::StreamProfile => {
                format!("Stream: {}", self.state.stream_profile().label())
            }
            ActionId::TogglePointer => format!(
                "Pointer scope: {}",
                if self.state.remote_pointer_requested() {
                    "enabled"
                } else {
                    "disabled"
                }
            ),
            ActionId::ToggleKeyboard => format!(
                "Keyboard scope: {}",
                if self.state.remote_keyboard_requested() {
                    "enabled"
                } else {
                    "disabled"
                }
            ),
            ActionId::GeneratePairingCode => "Generate pairing code".to_string(),
            ActionId::DisconnectSession => "Disconnect remote session".to_string(),
            ActionId::ForgetTrustedBrowsers => format!(
                "Forget trusted browsers ({})",
                self.state.trusted_browser_count()
            ),
            ActionId::PanicStop => "Panic stop".to_string(),
            ActionId::Quit => "Quit".to_string(),
        }
    }

    fn action_help(&self, action: ActionId) -> &'static str {
        match action {
            ActionId::RefreshNetwork => {
                "Refresh loopback and Tailscale access discovery without changing the host state."
            }
            ActionId::RefreshDisplays => {
                "Re-enumerate local displays and keep the current monitor selection valid."
            }
            ActionId::TailscaleUrl => {
                "Enable or refresh a private Tailscale Serve URL for the loopback-only browser client."
            }
            ActionId::Monitor => {
                "Use Left/Right or Enter to cycle through available displays for screen capture."
            }
            ActionId::StreamProfile => {
                "Use Left/Right or Enter to rotate through Balanced, Data Saver, and Emergency."
            }
            ActionId::TogglePointer => {
                "Allow or deny remote pointer movement, clicking, dragging, and scrolling."
            }
            ActionId::ToggleKeyboard => {
                "Allow or deny remote keyboard input, text entry, and shortcuts."
            }
            ActionId::GeneratePairingCode => {
                "Create a fresh one-time pairing code for the next browser session."
            }
            ActionId::DisconnectSession => {
                "Disconnect the currently approved browser session without changing input scopes."
            }
            ActionId::ForgetTrustedBrowsers => {
                "Revoke every remembered browser and clear the active remote session."
            }
            ActionId::PanicStop => {
                "Immediately disable remote input and clear the pairing code, active session, and every remembered browser."
            }
            ActionId::Quit => "Exit the host TUI and stop the host process.",
        }
    }

    fn remote_access_summary(&self) -> String {
        let mode = remote_access_mode_label(self.urls.tailscale_status.remote_access_mode());
        format!(
            "Access mode: {mode}   Preferred URL: {}",
            self.urls.preferred.url
        )
    }

    fn set_toast(&mut self, message: impl Into<String>) {
        self.toast = Some(Toast {
            message: message.into(),
            created_at: Instant::now(),
        });
    }
}

fn remote_access_mode_label(mode: RemoteAccessMode) -> &'static str {
    match mode {
        RemoteAccessMode::ReadyHttps => "Ready HTTPS",
        RemoteAccessMode::ReadyTailnet => "Ready Tailnet",
        RemoteAccessMode::NeedsTailscaleLogin => "Needs Tailscale Login",
        RemoteAccessMode::NeedsTailscaleInstall => "Needs Tailscale Install",
        RemoteAccessMode::LocalOnly => "Local Only",
    }
}

fn format_duration_compact(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let days = total_seconds / 86_400;
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if days > 0 {
        format!("{days}d {}h", (total_seconds % 86_400) / 3600)
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn format_bytes_compact(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    if bytes as f64 >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes as f64 >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
