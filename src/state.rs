use anyhow::{Result, anyhow};
use parking_lot::RwLock;
use std::{
    path::PathBuf,
    sync::{Arc, mpsc::Sender},
};

use crate::{
    config::{AppConfig, ConfigStore},
    input::InputCommand,
    model::{LatestFrame, MonitorInfo, StatusResponse},
    security::{PairCodeSnapshot, SessionAuthError, SessionGrant, SessionSnapshot, SessionStore},
};

pub struct AppState {
    config_store: ConfigStore,
    config: RwLock<AppConfig>,
    monitors: RwLock<Vec<MonitorInfo>>,
    latest_frame: RwLock<Option<LatestFrame>>,
    capture_error: RwLock<Option<String>>,
    input_tx: Sender<InputCommand>,
    sessions: RwLock<SessionStore>,
    is_elevated: bool,
}

impl AppState {
    pub fn new(
        config_store: ConfigStore,
        mut config: AppConfig,
        monitors: Vec<MonitorInfo>,
        input_tx: Sender<InputCommand>,
        is_elevated: bool,
    ) -> Result<Self> {
        config.normalize();

        Ok(Self {
            config_store,
            config: RwLock::new(config),
            monitors: RwLock::new(monitors),
            latest_frame: RwLock::new(None),
            capture_error: RwLock::new(None),
            input_tx,
            sessions: RwLock::new(SessionStore::new()),
            is_elevated,
        })
    }

    pub fn ensure_valid_selected_monitor(&self) -> Result<()> {
        let monitors = self.monitors();
        let monitor = preferred_monitor(self.selected_monitor_id(), &monitors)
            .ok_or_else(|| anyhow!("no display monitors were detected"))?;

        if Some(monitor.id) != self.selected_monitor_id() {
            self.set_selected_monitor(monitor.id)?;
        }

        Ok(())
    }

    pub fn port(&self) -> u16 {
        self.config.read().port
    }

    pub fn is_elevated(&self) -> bool {
        self.is_elevated
    }

    pub fn selected_monitor_id(&self) -> Option<u32> {
        self.config.read().selected_monitor_id
    }

    pub fn capture_settings(&self) -> (u8, u32) {
        let config = self.config.read();
        (
            config.jpeg_quality.max(76),
            config.max_frame_width.max(1800),
        )
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

    pub fn set_remote_pointer_enabled(&self, enabled: bool) -> Result<()> {
        if self.is_elevated && enabled {
            return Err(anyhow!(
                "remote pointer control stays disabled while the app is running as Administrator"
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
                "remote keyboard control stays disabled while the app is running as Administrator"
            ));
        }

        let mut config = self.config.write();
        config.remote_keyboard_enabled = enabled;
        self.config_store.save(&config)?;
        Ok(())
    }

    pub fn panic_stop(&self) -> Result<()> {
        {
            let mut config = self.config.write();
            config.remote_pointer_enabled = false;
            config.remote_keyboard_enabled = false;
            self.config_store.save(&config)?;
        }

        let mut sessions = self.sessions.write();
        sessions.clear_pair_code();
        sessions.clear_session();
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

    pub fn revoke_remote_session(&self) {
        self.sessions.write().clear_session();
    }

    pub fn issue_pairing_session(
        &self,
        code: &str,
        user_agent: Option<String>,
    ) -> Result<SessionGrant, crate::security::PairingError> {
        self.sessions.write().exchange_pair_code(code, user_agent)
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
                "remote input is locked because the Windows app is running as Administrator"
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
                    Err(anyhow!(
                        "remote pointer control is disabled on the Windows app"
                    ))
                }
            }
            InputCommand::Text { .. }
            | InputCommand::Key { .. }
            | InputCommand::Shortcut { .. } => {
                if self.remote_keyboard_enabled() {
                    Ok(())
                } else {
                    Err(anyhow!(
                        "remote keyboard, text, and shortcut input is disabled on the Windows app"
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

        StatusResponse {
            selected_monitor: self.selected_monitor(),
            monitors: self.monitors(),
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
                .map(|session| session.idle_expires_in.as_millis()),
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_store.path().to_path_buf()
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
