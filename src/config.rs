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

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Added to every reading (calibration), in dB.
    pub offset_db: f32,
    /// `"substring of the device friendly name" = "short label"`.
    pub labels: BTreeMap<String, String>,
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
        let lower = friendly_name.to_lowercase();
        self.labels
            .iter()
            .find(|(key, _)| lower.contains(&key.to_lowercase()))
            .map(|(_, label)| label.as_str())
    }
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
    }
}
