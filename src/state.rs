use anyhow::{Result, anyhow};
use parking_lot::{Condvar, Mutex, RwLock};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc::Sender},
    time::{Duration, Instant},
};

use crate::{
    config::{AppConfig, ConfigStore, StreamProfile, StreamSettings, normalize_device_code},
    fleet::FleetRegistry,
    input::InputCommand,
    model::{DeviceInfoResponse, LatestFrame, MonitorInfo, StatusResponse},
    oidc::OidcConfig,
    platform,
    security::{
        IssueAccessPasswordError, IssuePairingError, MAX_INPUTS_PER_SECOND, PairCodeSnapshot,
        SessionAuthError, SessionGrant, SessionSnapshot, SessionStore, TrustedBrowserAuthError,
        TrustedBrowserSnapshot, TrustedBrowserStore, access_password_config_from_plaintext,
        admin_token_matches_hash,
    },
};

struct MasterdaleInputWindow {
    started_at: Instant,
    count: u16,
}

pub struct RuntimeAuth {
    pub admin_token_hash: Option<String>,
    pub masterdale_token_hash: Option<String>,
    pub oidc: Option<OidcConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeRole {
    Standalone,
    Host,
    Agent,
}

pub struct AppState {
    config_store: ConfigStore,
    config: RwLock<AppConfig>,
    monitors: RwLock<Vec<MonitorInfo>>,
    latest_frame: RwLock<Option<LatestFrame>>,
    capture_error: RwLock<Option<String>>,
    input_tx: Sender<InputCommand>,
    sessions: RwLock<SessionStore>,
    is_elevated: bool,
    admin_token_hash: Option<String>,
    masterdale_token_hash: Option<String>,
    oidc: Option<OidcConfig>,
    masterdale_input_window: RwLock<MasterdaleInputWindow>,
    capture_active_until: Mutex<Instant>,
    capture_wake: Condvar,
    role: RuntimeRole,
    fleet: Option<Arc<FleetRegistry>>,
}

impl AppState {
    pub fn new(
        config_store: ConfigStore,
        mut config: AppConfig,
        monitors: Vec<MonitorInfo>,
        input_tx: Sender<InputCommand>,
        trusted_browser_store: TrustedBrowserStore,
        is_elevated: bool,
        auth: RuntimeAuth,
        role: RuntimeRole,
    ) -> Result<Self> {
        config.normalize();
        let fleet = matches!(role, RuntimeRole::Host).then(|| Arc::new(FleetRegistry::new()));

        Ok(Self {
            config_store,
            config: RwLock::new(config),
            monitors: RwLock::new(monitors),
            latest_frame: RwLock::new(None),
            capture_error: RwLock::new(None),
            input_tx,
            sessions: RwLock::new(SessionStore::new(trusted_browser_store)?),
            is_elevated,
            admin_token_hash: auth.admin_token_hash,
            masterdale_token_hash: auth.masterdale_token_hash,
            oidc: auth.oidc,
            masterdale_input_window: RwLock::new(MasterdaleInputWindow {
                started_at: Instant::now(),
                count: 0,
            }),
            capture_active_until: Mutex::new(Instant::now()),
            capture_wake: Condvar::new(),
            role,
            fleet,
        })
    }

    #[allow(dead_code)]
    pub fn role(&self) -> RuntimeRole {
        self.role
    }

    pub fn fleet_registry(&self) -> Option<Arc<FleetRegistry>> {
        self.fleet.clone()
    }

    pub fn fleet_enabled(&self) -> bool {
        self.fleet.is_some()
    }

    pub fn ensure_valid_selected_monitor(&self) -> Result<()> {
        let monitors = self.monitors();
        let Some(monitor) = preferred_monitor(self.selected_monitor_id(), &monitors) else {
            self.set_capture_error(
                "no display monitors were detected; desktop capture is unavailable",
            );
            return Ok(());
        };

        if Some(monitor.id) != self.selected_monitor_id() {
            self.set_selected_monitor(monitor.id)?;
        }

        Ok(())
    }

    pub fn port(&self) -> u16 {
        self.config.read().port
    }

    pub fn device_code(&self) -> String {
        self.config.read().device_code.clone()
    }

    pub fn access_password_configured(&self) -> bool {
        self.config.read().access_password.is_some()
    }

    pub fn is_elevated(&self) -> bool {
        self.is_elevated
    }

    pub fn admin_token_required(&self) -> bool {
        self.admin_token_hash.is_some()
    }

    pub fn authorize_admin_token(&self, token: Option<&str>) -> bool {
        match self.admin_token_hash.as_deref() {
            Some(token_hash) => token
                .map(|token| admin_token_matches_hash(token_hash, token))
                .unwrap_or(false),
            None => true,
        }
    }

    pub fn authorize_masterdale_token(&self, token: &str) -> bool {
        self.masterdale_token_hash
            .as_deref()
            .is_some_and(|token_hash| admin_token_matches_hash(token_hash, token))
    }

    pub fn authorize_masterdale_input(&self, token: &str) -> bool {
        if !self.authorize_masterdale_token(token) {
            return false;
        }

        let mut window = self.masterdale_input_window.write();
        if window.started_at.elapsed() >= Duration::from_secs(1) {
            window.started_at = Instant::now();
            window.count = 0;
        }
        if window.count >= MAX_INPUTS_PER_SECOND {
            return false;
        }
        window.count = window.count.saturating_add(1);
        true
    }

    pub fn oidc_config(&self) -> Option<&OidcConfig> {
        self.oidc.as_ref()
    }

    pub fn issue_identity_session(&self, user_agent: Option<String>) -> SessionGrant {
        self.sessions.write().issue_identity_session(user_agent)
    }

    pub fn note_viewer_activity(&self) {
        let now = Instant::now();
        let mut active_until = self.capture_active_until.lock();
        let was_inactive = *active_until <= now;
        *active_until = now + Duration::from_secs(3);
        if was_inactive {
            self.capture_wake.notify_one();
        }
    }

    pub fn wait_for_capture_demand(&self, interval: Duration) {
        let mut active_until = self.capture_active_until.lock();
        loop {
            let now = Instant::now();
            if *active_until > now {
                let remaining = active_until.saturating_duration_since(now);
                self.capture_wake
                    .wait_for(&mut active_until, interval.min(remaining));
                return;
            }
            self.capture_wake.wait(&mut active_until);
        }
    }

    pub fn selected_monitor_id(&self) -> Option<u32> {
        self.config.read().selected_monitor_id
    }

    pub fn stream_profile(&self) -> StreamProfile {
        self.config.read().stream_profile
    }

    pub fn stream_settings(&self) -> StreamSettings {
        self.stream_profile().settings()
    }

    pub fn capture_settings(&self) -> StreamSettings {
        let config = self.config.read();
        let mut settings = config.stream_profile.settings();
        settings.jpeg_quality = config.jpeg_quality.clamp(35, 90);
        settings.max_frame_width = config.max_frame_width.clamp(720, 1920);
        settings
    }

    pub fn remote_pointer_enabled(&self) -> bool {
        !self.is_elevated && self.config.read().remote_pointer_enabled
    }

    pub fn remote_keyboard_enabled(&self) -> bool {
        !self.is_elevated && self.config.read().remote_keyboard_enabled
    }

    pub fn remote_pointer_requested(&self) -> bool {
        self.config.read().remote_pointer_enabled
    }

    pub fn remote_keyboard_requested(&self) -> bool {
        self.config.read().remote_keyboard_enabled
    }

    pub fn set_selected_monitor(&self, monitor_id: u32) -> Result<()> {
        let mut config = self.config.write();
        config.selected_monitor_id = Some(monitor_id);
        self.config_store.save(&config)?;
        Ok(())
    }

    pub fn set_device_code(&self, device_code: &str) -> Result<String> {
        let device_code = normalize_device_code(device_code).ok_or_else(|| {
            anyhow!("device codes must be 4-32 ASCII letters, numbers, '-' or '_'")
        })?;
        let mut config = self.config.write();
        config.device_code = device_code.clone();
        self.config_store.save(&config)?;
        Ok(device_code)
    }

    pub fn set_access_password(&self, password: &str) -> Result<()> {
        let access_password = access_password_config_from_plaintext(password)
            .map_err(|err| anyhow!(err.to_string()))?;
        let mut config = self.config.write();
        config.access_password = Some(access_password);
        self.config_store.save(&config)?;
        Ok(())
    }

    pub fn clear_access_password(&self) -> Result<()> {
        let mut config = self.config.write();
        config.access_password = None;
        self.config_store.save(&config)?;
        Ok(())
    }

    pub fn set_stream_profile(&self, profile: StreamProfile) -> Result<()> {
        let mut config = self.config.write();
        config.apply_stream_profile(profile);
        self.config_store.save(&config)?;
        Ok(())
    }

    pub fn set_remote_pointer_enabled(&self, enabled: bool) -> Result<()> {
        if self.is_elevated && enabled {
            return Err(anyhow!(
                "remote pointer control stays disabled while the host runtime is elevated"
            ));
        }

        let mut config = self.config.write();
        config.remote_pointer_enabled = enabled;
        self.config_store.save(&config)?;
        Ok(())
    }

    pub fn set_remote_keyboard_enabled(&self, enabled: bool) -> Result<()> {
        if self.is_elevated && enabled {
            return Err(anyhow!(
                "remote keyboard control stays disabled while the host runtime is elevated"
            ));
        }

        let mut config = self.config.write();
        config.remote_keyboard_enabled = enabled;
        self.config_store.save(&config)?;
        Ok(())
    }

    pub fn enable_remote_control_for_paired_client(&self) -> Result<()> {
        if self.is_elevated {
            return Ok(());
        }

        let mut config = self.config.write();
        if config.remote_pointer_enabled && config.remote_keyboard_enabled {
            return Ok(());
        }

        config.remote_pointer_enabled = true;
        config.remote_keyboard_enabled = true;
        self.config_store.save(&config)?;
        Ok(())
    }

    pub fn panic_stop(&self) -> Result<()> {
        {
            let mut config = self.config.write();
            config.remote_pointer_enabled = false;
            config.remote_keyboard_enabled = false;
            config.access_password = None;
            self.config_store.save(&config)?;
        }

        let mut sessions = self.sessions.write();
        sessions.clear_pair_code();
        sessions.clear_trusted_browsers()?;
        Ok(())
    }

    pub fn generate_pair_code(&self) -> PairCodeSnapshot {
        self.sessions.write().generate_pair_code()
    }

    pub fn current_pair_code(&self) -> Option<PairCodeSnapshot> {
        self.sessions.write().pair_code_snapshot()
    }

    pub fn current_remote_session(&self) -> Option<SessionSnapshot> {
        self.sessions.write().session_snapshot()
    }

    pub fn current_remote_user_agent(&self) -> Option<String> {
        self.sessions
            .read()
            .current_user_agent()
            .map(ToString::to_string)
    }

    pub fn trusted_browser_snapshots(&self) -> Vec<TrustedBrowserSnapshot> {
        self.sessions.read().trusted_browser_snapshots()
    }

    pub fn trusted_browser_count(&self) -> usize {
        self.sessions.read().trusted_browser_count()
    }

    pub fn revoke_remote_session(&self) {
        self.sessions.write().clear_session();
    }

    pub fn revoke_trusted_browsers(&self) -> Result<usize> {
        self.sessions.write().clear_trusted_browsers()
    }

    pub fn issue_pairing_session(
        &self,
        code: &str,
        user_agent: Option<String>,
        remember_browser: bool,
    ) -> Result<SessionGrant, IssuePairingError> {
        self.sessions
            .write()
            .issue_pairing_session(code, user_agent, remember_browser)
    }

    pub fn issue_access_password_session(
        &self,
        device_code: &str,
        password: &str,
        user_agent: Option<String>,
        remember_browser: bool,
    ) -> Result<SessionGrant, IssueAccessPasswordError> {
        let config = self.config.read();
        self.sessions.write().issue_access_password_session(
            device_code,
            &config.device_code,
            password,
            config.access_password.as_ref(),
            user_agent,
            remember_browser,
        )
    }

    pub fn restore_trusted_browser_session(
        &self,
        trusted_token: &str,
        user_agent: Option<String>,
    ) -> Result<SessionGrant, TrustedBrowserAuthError> {
        self.sessions
            .write()
            .restore_trusted_browser_session(trusted_token, user_agent)
    }

    pub fn authorize_session(&self, session_id: &str) -> Result<SessionSnapshot, SessionAuthError> {
        self.sessions.write().authorize_session(session_id)
    }

    pub fn authorize_input_session(
        &self,
        session_id: &str,
    ) -> Result<SessionSnapshot, SessionAuthError> {
        self.sessions.write().authorize_input_session(session_id)
    }

    pub fn record_status_response(&self, session_id: &str, bytes_sent: usize) {
        if let Err(err) = self
            .sessions
            .write()
            .record_status_response(session_id, bytes_sent)
        {
            tracing::debug!(error = ?err, "Failed to record session status transfer");
        }
    }

    pub fn record_frame_response(
        &self,
        session_id: &str,
        bytes_sent: usize,
        reused_cached_frame: bool,
    ) {
        if let Err(err) =
            self.sessions
                .write()
                .record_frame_response(session_id, bytes_sent, reused_cached_frame)
        {
            tracing::debug!(error = ?err, "Failed to record session frame transfer");
        }
    }

    pub fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.read().clone()
    }

    pub fn set_monitors(&self, monitors: Vec<MonitorInfo>) {
        *self.monitors.write() = monitors;
    }

    pub fn selected_monitor(&self) -> Option<MonitorInfo> {
        let monitors = self.monitors.read();
        preferred_monitor(self.selected_monitor_id(), &monitors)
    }

    pub fn latest_frame(&self) -> Option<LatestFrame> {
        self.latest_frame.read().clone()
    }

    pub fn update_frame(&self, frame: LatestFrame) {
        *self.latest_frame.write() = Some(frame);
        self.clear_capture_error();
    }

    pub fn set_capture_error(&self, message: impl Into<String>) {
        *self.capture_error.write() = Some(message.into());
    }

    pub fn clear_capture_error(&self) {
        self.capture_error.write().take();
    }

    pub fn capture_error(&self) -> Option<String> {
        self.capture_error.read().clone()
    }

    pub fn ensure_remote_command_allowed(&self, command: &InputCommand) -> Result<()> {
        if self.is_elevated {
            return Err(anyhow!(
                "remote input is locked because the host runtime is running with elevated privileges"
            ));
        }

        match command {
            InputCommand::Move { .. }
            | InputCommand::Click { .. }
            | InputCommand::Button { .. }
            | InputCommand::Scroll { .. } => {
                if self.remote_pointer_enabled() {
                    Ok(())
                } else {
                    Err(anyhow!("remote pointer control is disabled on the host"))
                }
            }
            InputCommand::Text { .. }
            | InputCommand::Key { .. }
            | InputCommand::Shortcut { .. } => {
                if self.remote_keyboard_enabled() {
                    Ok(())
                } else {
                    Err(anyhow!(
                        "remote keyboard, text, and shortcut input is disabled on the host"
                    ))
                }
            }
        }
    }

    pub fn send_input(&self, command: InputCommand) -> Result<()> {
        self.ensure_remote_command_allowed(&command)?;
        self.input_tx
            .send(command)
            .map_err(|_| anyhow!("the input worker is no longer available"))
    }

    pub fn status_response(&self) -> StatusResponse {
        let frame = self.latest_frame();
        let session = self.current_remote_session();
        let stream_settings = self.stream_settings();

        StatusResponse {
            selected_monitor: self.selected_monitor(),
            monitors: self.monitors(),
            stream_profile: self.stream_profile(),
            active_frame_interval_ms: stream_settings.active_frame_interval.as_millis() as u64,
            idle_frame_interval_ms: stream_settings.idle_frame_interval.as_millis() as u64,
            interaction_boost_window_ms: stream_settings.interaction_boost_window.as_millis()
                as u64,
            has_frame: frame.is_some(),
            frame_width: frame.as_ref().map(|frame| frame.encoded_width),
            frame_height: frame.as_ref().map(|frame| frame.encoded_height),
            source_width: frame.as_ref().map(|frame| frame.source_width),
            source_height: frame.as_ref().map(|frame| frame.source_height),
            last_frame_age_ms: frame
                .as_ref()
                .and_then(|frame| frame.captured_at.elapsed().ok())
                .map(|elapsed| elapsed.as_millis()),
            capture_error: self.capture_error(),
            remote_pointer_enabled: self.remote_pointer_enabled(),
            remote_keyboard_enabled: self.remote_keyboard_enabled(),
            host_elevated: self.is_elevated,
            session_expires_in_ms: session
                .as_ref()
                .map(|session| session.expires_in.as_millis()),
            session_idle_expires_in_ms: session
                .as_ref()
                .and_then(|session| session.idle_expires_in.map(|duration| duration.as_millis())),
            session_bytes_sent: session.as_ref().map(|session| session.bytes_sent),
            session_frame_responses: session.as_ref().map(|session| session.frame_responses),
            session_cached_frame_hits: session.as_ref().map(|session| session.cached_frame_hits),
            session_status_responses: session.as_ref().map(|session| session.status_responses),
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_store.path().to_path_buf()
    }

    pub fn device_info_response(&self) -> DeviceInfoResponse {
        let username = platform::username();
        let hostname = platform::hostname();
        let display_name = match (username.as_deref(), hostname.as_deref()) {
            (Some(username), Some(hostname)) => format!("{username}@{hostname}"),
            (Some(username), None) => username.to_string(),
            (None, Some(hostname)) => hostname.to_string(),
            (None, None) => "RustOp host".to_string(),
        };

        DeviceInfoResponse {
            device_code: self.device_code(),
            username,
            hostname,
            display_name,
            os: platform::os_label(),
            os_family: platform::os_family(),
            password_enabled: self.access_password_configured(),
            oidc_enabled: self.oidc.is_some(),
            fleet_enabled: self.fleet_enabled(),
            role: match self.role {
                RuntimeRole::Standalone => "standalone".to_string(),
                RuntimeRole::Host => "host".to_string(),
                RuntimeRole::Agent => "agent".to_string(),
            },
        }
    }
}

pub fn preferred_monitor(
    selected_monitor_id: Option<u32>,
    monitors: &[MonitorInfo],
) -> Option<MonitorInfo> {
    selected_monitor_id
        .and_then(|selected| monitors.iter().find(|monitor| monitor.id == selected))
        .cloned()
        .or_else(|| monitors.iter().find(|monitor| monitor.is_primary).cloned())
        .or_else(|| monitors.first().cloned())
}

#[allow(dead_code)]
pub type SharedState = Arc<AppState>;
