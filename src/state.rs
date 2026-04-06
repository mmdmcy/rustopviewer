use anyhow::{Result, anyhow};
use parking_lot::RwLock;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
};

use crate::{
    config::{AppConfig, ConfigStore},
    input::InputCommand,
    model::{LatestFrame, MonitorInfo, StatusResponse},
};

pub struct AppState {
    config_store: ConfigStore,
    config: RwLock<AppConfig>,
    monitors: RwLock<Vec<MonitorInfo>>,
    latest_frame: RwLock<Option<LatestFrame>>,
    capture_error: RwLock<Option<String>>,
    input_tx: Sender<InputCommand>,
    restart_requested: AtomicBool,
}

impl AppState {
    pub fn new(
        config_store: ConfigStore,
        mut config: AppConfig,
        monitors: Vec<MonitorInfo>,
        input_tx: Sender<InputCommand>,
    ) -> Result<Self> {
        config.normalize();

        Ok(Self {
            config_store,
            config: RwLock::new(config),
            monitors: RwLock::new(monitors),
            latest_frame: RwLock::new(None),
            capture_error: RwLock::new(None),
            input_tx,
            restart_requested: AtomicBool::new(false),
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

    pub fn auth_token(&self) -> String {
        self.config.read().auth_token.clone()
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

    pub fn set_selected_monitor(&self, monitor_id: u32) -> Result<()> {
        let mut config = self.config.write();
        config.selected_monitor_id = Some(monitor_id);
        self.config_store.save(&config)?;
        Ok(())
    }

    pub fn regenerate_auth_token(&self) -> Result<String> {
        let mut config = self.config.write();
        let token = config.regenerate_auth_token();
        self.config_store.save(&config)?;
        Ok(token)
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

    pub fn send_input(&self, command: InputCommand) -> Result<()> {
        self.input_tx
            .send(command)
            .map_err(|_| anyhow!("the input worker is no longer available"))
    }

    pub fn status_response(&self) -> StatusResponse {
        let frame = self.latest_frame();

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
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_store.path().to_path_buf()
    }

    pub fn request_restart(&self) {
        self.restart_requested.store(true, Ordering::SeqCst);
    }

    pub fn take_restart_requested(&self) -> bool {
        self.restart_requested.swap(false, Ordering::SeqCst)
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
