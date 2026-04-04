use anyhow::{Context, Result};
use directories::ProjectDirs;
use rand::distr::{Alphanumeric, SampleString};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

const DEFAULT_PORT: u16 = 45080;
const DEFAULT_JPEG_QUALITY: u8 = 78;
const DEFAULT_MAX_FRAME_WIDTH: u32 = 1800;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub port: u16,
    pub auth_token: String,
    pub selected_monitor_id: Option<u32>,
    pub jpeg_quality: u8,
    pub max_frame_width: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            auth_token: generate_auth_token(),
            selected_monitor_id: None,
            jpeg_quality: DEFAULT_JPEG_QUALITY,
            max_frame_width: DEFAULT_MAX_FRAME_WIDTH,
        }
    }
}

impl AppConfig {
    pub fn normalize(&mut self) {
        if self.port == 0 {
            self.port = DEFAULT_PORT;
        }

        if self.auth_token.trim().is_empty() {
            self.auth_token = generate_auth_token();
        }

        self.jpeg_quality = self.jpeg_quality.clamp(35, 90);
        self.max_frame_width = self.max_frame_width.clamp(720, 1920);
    }

    pub fn regenerate_auth_token(&mut self) -> String {
        self.auth_token = generate_auth_token();
        self.auth_token.clone()
    }
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new() -> Result<Self> {
        let project_dirs = ProjectDirs::from("io", "jelle", "RustOpViewer")
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
            let mut config: AppConfig = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", self.path.display()))?;
            let previous = config.clone();
            config.normalize();

            if config != previous {
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
}

fn generate_auth_token() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 32)
}
