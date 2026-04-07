use serde::Serialize;
use std::{sync::Arc, time::SystemTime};

use crate::config::StreamProfile;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

impl MonitorInfo {
    pub fn display_name(&self) -> String {
        if self.is_primary {
            format!("{} (primary)", self.name)
        } else {
            self.name.clone()
        }
    }

    pub fn resolution_label(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }
}

#[derive(Debug, Clone)]
pub struct LatestFrame {
    pub jpeg: Arc<Vec<u8>>,
    pub etag: String,
    pub byte_len: usize,
    pub source_width: u32,
    pub source_height: u32,
    pub encoded_width: u32,
    pub encoded_height: u32,
    pub captured_at: SystemTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    pub selected_monitor: Option<MonitorInfo>,
    pub monitors: Vec<MonitorInfo>,
    pub stream_profile: StreamProfile,
    pub active_frame_interval_ms: u64,
    pub idle_frame_interval_ms: u64,
    pub interaction_boost_window_ms: u64,
    pub has_frame: bool,
    pub frame_width: Option<u32>,
    pub frame_height: Option<u32>,
    pub source_width: Option<u32>,
    pub source_height: Option<u32>,
    pub last_frame_age_ms: Option<u128>,
    pub capture_error: Option<String>,
    pub remote_pointer_enabled: bool,
    pub remote_keyboard_enabled: bool,
    pub host_elevated: bool,
    pub session_expires_in_ms: Option<u128>,
    pub session_idle_expires_in_ms: Option<u128>,
    pub session_bytes_sent: Option<u64>,
    pub session_frame_responses: Option<u64>,
    pub session_cached_frame_hits: Option<u64>,
    pub session_status_responses: Option<u64>,
}
