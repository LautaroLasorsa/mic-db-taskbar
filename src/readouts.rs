//! One smoothed readout per active capture device, kept in sync with the device list.

use std::time::{Duration, Instant};

use crate::alarm::Alarm;
use crate::audio::{FLOOR_DB, Meter};
use crate::config::Config;
use crate::devices::{self, DeviceInfo};
use crate::history::History;

/// A device that could not be opened (exclusive use, invalidated) is retried after this long.
const RETRY_AFTER: Duration = Duration::from_secs(15);
/// Shown instead of a level while a device cannot be captured.
const UNAVAILABLE: &str = "\u{2014}";

/// Fraction of the gap closed per tick when the level rises (fast attack).
const ATTACK: f32 = 0.7;
/// Fraction of the gap closed per tick when the level falls (slow release).
const RELEASE: f32 = 0.15;
const SEPARATOR: &str = "  \u{2502}  ";
const FIGURE_SPACE: char = '\u{2007}';
const MINUS: char = '\u{2212}';

struct Readout {
    device_id: String,
    label: String,
    alarm_db: f32,
    meter: Meter,
    started: Instant,
    displayed_db: f32,
    alarm: Alarm,
    history: History,
}

impl Readout {
    fn new(device: &DeviceInfo, config: &Config) -> Self {
        let label = config.label_for(&device.friendly_name).map_or_else(
            || devices::short_label(&device.friendly_name),
            str::to_owned,
        );
        Self {
            device_id: device.id.clone(),
            label,
            alarm_db: config.alarm_for(&device.friendly_name),
            meter: Meter::start(&device.id),
            started: Instant::now(),
            displayed_db: FLOOR_DB,
            alarm: Alarm::default(),
            history: History::default(),
        }
    }

    fn should_restart(&self) -> bool {
        self.meter.has_failed() && self.started.elapsed() >= RETRY_AFTER
    }

    /// Smoothed level in dBFS (before offset), or `None` while not capturing.
    fn smooth(&mut self) -> Option<f32> {
        let level = self.meter.level_db()?;
        let rate = if level > self.displayed_db {
            ATTACK
        } else {
            RELEASE
        };
        self.displayed_db += (level - self.displayed_db) * rate;
        Some(self.displayed_db)
    }

    /// Advances one tick and returns this device's text segments.
    fn tick(&mut self, offset_db: f32, now: Instant) -> [Segment; 2] {
        let level = self.smooth().map(|db| db + offset_db);
        self.history.record(now, level);
        let alarmed = self
            .alarm
            .update(now, level.is_some_and(|db| db > self.alarm_db));
        let value = level.map_or_else(
            || UNAVAILABLE.to_owned(),
            |db| format!("{} dB", format_db(db)),
        );
        [
            Segment::plain(format!("{} ", self.label)),
            Segment {
                text: value,
                alarm: alarmed && level.is_some(),
            },
        ]
    }
}

/// A run of overlay text; `alarm` segments are drawn in red.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub alarm: bool,
}

impl Segment {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            alarm: false,
        }
    }
}

pub struct Series<'a> {
    pub label: &'a str,
    pub alarm_db: f32,
    pub history: &'a History,
}

#[derive(Default)]
pub struct Readouts {
    items: Vec<Readout>,
}

impl Readouts {
    /// Keeps meters for devices still present, starts new ones, restarts failed ones after a
    /// back-off, and skips excluded devices entirely.
    pub fn sync(&mut self, devices: &[DeviceInfo], config: &Config) {
        let mut previous = std::mem::take(&mut self.items);
        self.items = devices
            .iter()
            .filter(|device| !config.is_excluded(&device.friendly_name, device.bluetooth))
            .map(
                |device| match previous.iter().position(|r| r.device_id == device.id) {
                    Some(index) if !previous[index].should_restart() => previous.swap_remove(index),
                    _ => Readout::new(device, config),
                },
            )
            .collect();
    }

    /// Advances smoothing/alarms one tick, records history, and returns the taskbar text,
    /// e.g. `Realtek −42 dB  │  USB —`.
    pub fn tick(&mut self, offset_db: f32, now: Instant) -> Vec<Segment> {
        if self.items.is_empty() {
            return vec![Segment::plain("No mic")];
        }
        let mut segments = Vec::with_capacity(self.items.len() * 3);
        for (index, readout) in self.items.iter_mut().enumerate() {
            if index > 0 {
                segments.push(Segment::plain(SEPARATOR));
            }
            segments.extend(readout.tick(offset_db, now));
        }
        segments
    }

    /// Per-device data for the history chart, in display order.
    pub fn series(&self) -> Vec<Series<'_>> {
        self.items
            .iter()
            .map(|readout| Series {
                label: &readout.label,
                alarm_db: readout.alarm_db,
                history: &readout.history,
            })
            .collect()
    }
}

/// Formats a level for the chart legend (no padding).
pub fn format_level(db: Option<f32>) -> String {
    db.map_or_else(
        || UNAVAILABLE.to_owned(),
        |db| format!("{} dB", format_db(db).trim_start_matches(FIGURE_SPACE)),
    )
}

/// Rounds to an integer with a typographic minus, padded to a stable width.
fn format_db(db: f32) -> String {
    let value = db.round() as i32;
    let digits = value.unsigned_abs().to_string();
    let signed = if value < 0 {
        format!("{MINUS}{digits}")
    } else {
        digits
    };
    let pad = 3usize.saturating_sub(signed.chars().count());
    std::iter::repeat_n(FIGURE_SPACE, pad)
        .chain(signed.chars())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_db_rounds_and_pads() {
        assert_eq!(format_db(-42.4), "\u{2212}42");
        assert_eq!(format_db(-4.6), "\u{2007}\u{2212}5");
        assert_eq!(format_db(0.2), "\u{2007}\u{2007}0");
    }
}
