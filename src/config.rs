//! Settings persistence

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub pulses: i32,
    pub gap_deg: f64,
    pub image_size: i32,
    pub colormap: String,
    pub jobs: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            pulses: 720,
            gap_deg: 1.0,
            image_size: 1735,
            colormap: "viridis".to_string(),
            jobs: 0,
        }
    }
}


fn settings_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "imsel", "radar_plotter")
        .map(|dirs| dirs.config_dir().join("settings.json"))
}

pub fn load_settings() -> Result<Settings, Box<dyn std::error::Error>> {
    let path = settings_path().ok_or("Could not determine config directory")?;
    let content = std::fs::read_to_string(path)?;
    let settings: Settings = serde_json::from_str(&content)?;
    Ok(settings)
}

pub fn save_settings(settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    let path = settings_path().ok_or("Could not determine config directory")?;
    
    // Create parent directory if it doesn't exist
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    
    let content = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, content)?;
    Ok(())
}
