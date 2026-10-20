//! Optional TOML configuration.
//!
//! Looked up first next to the executable (`mic-db-taskbar.toml`), then in
//! `%APPDATA%\mic-db-taskbar\config.toml`. A missing or invalid file yields defaults.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

const FILE_NAME_NEXT_TO_EXE: &str = "mic-db-taskbar.toml";
const APPDATA_DIR: &str = "mic-db-taskbar";
const APPDATA_FILE: &str = "config.toml";

const DEFAULT_ALARM_DB: f32 = -40.0;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Added to every reading (calibration), in dB.
    pub offset_db: f32,
    /// Level (after `offset_db`) above which a readout turns red.
    pub alarm_db: f32,
    /// `"substring of the device friendly name" = "short label"`.
    pub labels: BTreeMap<String, String>,
    /// `"substring of the device friendly name" = alarm threshold in dB`.
    pub alarms: BTreeMap<String, f32>,
    /// Substrings of device friendly names that are never opened (e.g. Bluetooth headsets).
    pub exclude: Vec<String>,
    /// Bluetooth mics are skipped unless this is true: opening one forces the headset into
    /// the low-quality hands-free profile.
    pub include_bluetooth: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            offset_db: 0.0,
            alarm_db: DEFAULT_ALARM_DB,
            labels: BTreeMap::new(),
            alarms: BTreeMap::new(),
            exclude: Vec::new(),
            include_bluetooth: false,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        candidate_paths()
            .into_iter()
            .find_map(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Returns the user-defined label for a device whose friendly name contains a configured key.
    pub fn label_for(&self, friendly_name: &str) -> Option<&str> {
        find_by_substring(&self.labels, friendly_name).map(String::as_str)
    }

    /// Alarm threshold for a device: its override if configured, else `alarm_db`.
    pub fn alarm_for(&self, friendly_name: &str) -> f32 {
        find_by_substring(&self.alarms, friendly_name)
            .copied()
            .unwrap_or(self.alarm_db)
    }

    /// Whether a device must not be opened at all.
    pub fn is_excluded(&self, friendly_name: &str, bluetooth: bool) -> bool {
        if bluetooth && !self.include_bluetooth {
            return true;
        }
        let lower = friendly_name.to_lowercase();
        self.exclude
            .iter()
            .any(|key| lower.contains(&key.to_lowercase()))
    }
}

/// Case-insensitive "key is a substring of the name" lookup.
fn find_by_substring<'a, V>(map: &'a BTreeMap<String, V>, friendly_name: &str) -> Option<&'a V> {
    let lower = friendly_name.to_lowercase();
    map.iter()
        .find(|(key, _)| lower.contains(&key.to_lowercase()))
        .map(|(_, value)| value)
}

fn candidate_paths() -> Vec<PathBuf> {
    let next_to_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(FILE_NAME_NEXT_TO_EXE)));
    let in_appdata = std::env::var_os("APPDATA")
        .map(|dir| PathBuf::from(dir).join(APPDATA_DIR).join(APPDATA_FILE));
    next_to_exe.into_iter().chain(in_appdata).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_offset_and_labels_case_insensitively() {
        let cfg: Config =
            toml::from_str("offset_db = -3.5\n[labels]\n\"realtek\" = \"Int\"\n").expect("valid");
        assert_eq!(cfg.offset_db, -3.5);
        assert_eq!(cfg.label_for("Microphone (Realtek(R) Audio)"), Some("Int"));
        assert_eq!(cfg.label_for("USB Mic"), None);
        assert_eq!(cfg.alarm_db, DEFAULT_ALARM_DB);
    }

    #[test]
    fn alarm_overrides_and_exclusions() {
        let cfg: Config = toml::from_str(
            "alarm_db = -30\nexclude = [\"hands-free\"]\n[alarms]\n\"usb\" = -50.0\n",
        )
        .expect("valid");
        assert_eq!(cfg.alarm_for("Microphone (USB Audio)"), -50.0);
        assert_eq!(cfg.alarm_for("Realtek"), -30.0);
        assert!(cfg.is_excluded("Headset (WH-1000XM4 Hands-Free)", false));
        assert!(!cfg.is_excluded("USB", false));
        assert!(cfg.is_excluded("USB", true), "bluetooth skipped by default");
    }
}
