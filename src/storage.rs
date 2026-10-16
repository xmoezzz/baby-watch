use std::{fs, path::PathBuf};

use chrono::{DateTime, Local};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::{
    model::WatchItem,
    release::{ReleaseRule, ReleaseSchedule},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub poll_seconds: u64,
    pub notifications: bool,
    pub compact_mode: bool,
    pub catalog_monitor: bool,
    pub armed_poll_seconds: u64,
    pub browser: BrowserChoice,
    pub release_schedule: ReleaseSchedule,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            poll_seconds: 30,
            notifications: true,
            compact_mode: false,
            catalog_monitor: true,
            armed_poll_seconds: 8,
            browser: BrowserChoice::SystemDefault,
            release_schedule: ReleaseSchedule::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum BrowserChoice {
    #[default]
    SystemDefault,
    Safari,
    Chrome,
}

impl BrowserChoice {
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemDefault => "System default",
            Self::Safari => "Safari",
            Self::Chrome => "Google Chrome",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedState {
    pub watch_items: Vec<WatchItem>,
    pub release_rules: Vec<ReleaseRule>,
    pub settings: Settings,
    /// Legacy v0.5 field; retained so existing state files migrate cleanly.
    pub login_last_confirmed_at: Option<DateTime<Local>>,
    pub japan_login_last_confirmed_at: Option<DateTime<Local>>,
    pub china_login_last_confirmed_at: Option<DateTime<Local>>,
}

pub fn load() -> PersistedState {
    let Some(path) = state_path() else {
        return PersistedState::default();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return PersistedState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(state: &PersistedState) -> std::io::Result<()> {
    let Some(path) = state_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_owned());
    fs::write(path, text)
}

fn state_path() -> Option<PathBuf> {
    let dirs = ProjectDirs::from("com", "baby-watch", "BABY Watch")?;
    Some(dirs.config_dir().join("state.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrates_v06_without_enabling_release() {
        let state: PersistedState =
            serde_json::from_str(r#"{"watch_items":[],"settings":{"poll_seconds":60}}"#).unwrap();
        assert_eq!(state.settings.poll_seconds, 60);
        assert!(!state.settings.release_schedule.enabled);
        assert!(state.release_rules.is_empty());
    }
}
