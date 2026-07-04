use anyhow::{Context, Result};
use directories::ProjectDirs;
use rand::{distr::Alphanumeric, distr::SampleString};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const DEFAULT_PORT: u16 = 45080;
const DEFAULT_DEVICE_CODE_LEN: usize = 9;
const MIN_DEVICE_CODE_LEN: usize = 4;
const MAX_DEVICE_CODE_LEN: usize = 32;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamProfile {
    #[default]
    Balanced,
    DataSaver,
    Emergency,
}

#[derive(Debug, Clone, Copy)]
pub struct StreamSettings {
    pub jpeg_quality: u8,
    pub max_frame_width: u32,
    pub capture_interval: Duration,
    pub active_frame_interval: Duration,
    pub idle_frame_interval: Duration,
    pub interaction_boost_window: Duration,
}

impl StreamProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::Balanced => "Balanced",
            Self::DataSaver => "Data Saver",
            Self::Emergency => "Emergency",
        }
    }

    pub fn summary(self) -> &'static str {
        match self {
            Self::Balanced => "Readable desktop with moderate mobile data use.",
            Self::DataSaver => "Lower bandwidth while keeping desktop text usable.",
            Self::Emergency => "Readable-first fallback for tight or flaky mobile data.",
        }
    }

    pub fn settings(self) -> StreamSettings {
        match self {
            Self::Balanced => StreamSettings {
                jpeg_quality: 68,
                max_frame_width: 1400,
                capture_interval: Duration::from_millis(220),
                active_frame_interval: Duration::from_millis(250),
                idle_frame_interval: Duration::from_millis(500),
                interaction_boost_window: Duration::from_millis(2_000),
            },
            Self::DataSaver => StreamSettings {
                jpeg_quality: 52,
                max_frame_width: 960,
                capture_interval: Duration::from_millis(320),
                active_frame_interval: Duration::from_millis(400),
                idle_frame_interval: Duration::from_millis(850),
                interaction_boost_window: Duration::from_millis(2_400),
            },
            Self::Emergency => StreamSettings {
                jpeg_quality: 40,
                max_frame_width: 720,
                capture_interval: Duration::from_millis(420),
                active_frame_interval: Duration::from_millis(320),
                idle_frame_interval: Duration::from_millis(1_100),
                interaction_boost_window: Duration::from_millis(2_800),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub port: u16,
    #[serde(default = "generate_device_code")]
    pub device_code: String,
    pub access_password: Option<AccessPasswordConfig>,
    pub selected_monitor_id: Option<u32>,
    pub stream_profile: StreamProfile,
    pub jpeg_quality: u8,
    pub max_frame_width: u32,
    pub remote_pointer_enabled: bool,
    pub remote_keyboard_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        let defaults = StreamProfile::default().settings();
        Self {
            port: DEFAULT_PORT,
            device_code: generate_device_code(),
            access_password: None,
            selected_monitor_id: None,
            stream_profile: StreamProfile::default(),
            jpeg_quality: defaults.jpeg_quality,
            max_frame_width: defaults.max_frame_width,
            remote_pointer_enabled: true,
            remote_keyboard_enabled: true,
        }
    }
}

impl AppConfig {
    pub fn normalize(&mut self) {
        if self.port == 0 {
            self.port = DEFAULT_PORT;
        }

        if let Some(device_code) = normalize_device_code(&self.device_code) {
            self.device_code = device_code;
        } else {
            self.device_code = generate_device_code();
        }

        self.apply_stream_profile_bounds();
    }

    pub fn apply_stream_profile(&mut self, profile: StreamProfile) {
        self.stream_profile = profile;
        let settings = profile.settings();
        self.jpeg_quality = settings.jpeg_quality;
        self.max_frame_width = settings.max_frame_width;
        self.apply_stream_profile_bounds();
    }

    fn apply_stream_profile_bounds(&mut self) {
        self.jpeg_quality = self.jpeg_quality.clamp(35, 90);
        self.max_frame_width = self.max_frame_width.clamp(720, 1920);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessPasswordConfig {
    pub salt: String,
    pub password_hash: String,
}

pub fn generate_device_code() -> String {
    let code = Alphanumeric.sample_string(&mut rand::rng(), DEFAULT_DEVICE_CODE_LEN);
    normalize_device_code(&code).unwrap_or_else(|| "ROV-HOST".to_string())
}

pub fn normalize_device_code(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_uppercase())
            } else if matches!(character, '-' | '_') {
                Some(character)
            } else {
                None
            }
        })
        .take(MAX_DEVICE_CODE_LEN)
        .collect::<String>();

    if normalized.len() >= MIN_DEVICE_CODE_LEN {
        Some(normalized)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new() -> Result<Self> {
        let project_dirs = ProjectDirs::from("io", "RustOpViewer", "RustOpViewer")
            .context("failed to resolve the app config directory")?;

        fs::create_dir_all(project_dirs.config_dir()).with_context(|| {
            format!(
                "failed to create config directory at {}",
                project_dirs.config_dir().display()
            )
        })?;

        Ok(Self {
            path: project_dirs.config_dir().join("config.json"),
        })
    }

    pub fn load_or_create(&self) -> Result<AppConfig> {
        if self.path.exists() {
            let content = fs::read_to_string(&self.path)
                .with_context(|| format!("failed to read {}", self.path.display()))?;
            let missing_device_code = !content.contains("\"device_code\"");
            let mut config: AppConfig = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", self.path.display()))?;
            let previous = config.clone();
            config.normalize();

            if missing_device_code || config != previous {
                self.save(&config)?;
            }

            Ok(config)
        } else {
            let config = AppConfig::default();
            self.save(&config)?;
            Ok(config)
        }
    }

    pub fn save(&self, config: &AppConfig) -> Result<()> {
        let serialized =
            serde_json::to_string_pretty(config).context("failed to serialize config")?;
        fs::write(&self.path, serialized)
            .with_context(|| format!("failed to write {}", self.path.display()))?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn trusted_browsers_path(&self) -> PathBuf {
        self.path.with_file_name("trusted-browsers.json")
    }
}

#[cfg(test)]
mod tests {
    use super::ConfigStore;
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    fn temp_config_store(name: &str) -> ConfigStore {
        let unique = format!(
            "rustopviewer-config-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        fs::create_dir_all(&directory).expect("temp config directory should be created");
        ConfigStore {
            path: directory.join("config.json"),
        }
    }

    #[test]
    fn loading_old_config_persists_generated_device_code() {
        let store = temp_config_store("device-code");
        fs::write(
            &store.path,
            r#"{
  "port": 45080,
  "selected_monitor_id": null,
  "stream_profile": "balanced",
  "jpeg_quality": 68,
  "max_frame_width": 1400,
  "remote_pointer_enabled": true,
  "remote_keyboard_enabled": true
}"#,
        )
        .expect("old config should be written");

        let config = store
            .load_or_create()
            .expect("old config should load with a generated device code");
        assert!(!config.device_code.is_empty());
        let persisted = fs::read_to_string(&store.path).expect("config should be saved");
        assert!(persisted.contains("\"device_code\""));

        let reloaded = store
            .load_or_create()
            .expect("migrated config should reload");
        assert_eq!(reloaded.device_code, config.device_code);

        if let Some(parent) = store.path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}
