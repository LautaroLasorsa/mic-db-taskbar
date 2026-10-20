//! One smoothed readout per active capture device, kept in sync with the device list.

use crate::audio::{FLOOR_DB, Meter};
use crate::config::Config;
use crate::devices::{self, DeviceInfo};

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
    meter: Meter,
    displayed_db: f32,
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
            meter: Meter::start(&device.id),
            displayed_db: FLOOR_DB,
        }
    }

    fn tick(&mut self) -> Option<f32> {
        let level = self.meter.level_db()?;
        let rate = if level > self.displayed_db {
            ATTACK
        } else {
            RELEASE
        };
        self.displayed_db += (level - self.displayed_db) * rate;
        Some(self.displayed_db)
    }
}

#[derive(Default)]
pub struct Readouts {
    items: Vec<Readout>,
}

impl Readouts {
    /// Keeps meters for devices still present, starts new ones, and restarts failed ones.
    pub fn sync(&mut self, devices: &[DeviceInfo], config: &Config) {
        let mut previous = std::mem::take(&mut self.items);
        self.items = devices
            .iter()
            .map(
                |device| match previous.iter().position(|r| r.device_id == device.id) {
                    Some(index) if !previous[index].meter.has_failed() => {
                        previous.swap_remove(index)
                    }
                    _ => Readout::new(device, config),
                },
            )
            .collect();
    }

    /// Advances smoothing one tick and formats e.g. `"Realtek −42 dB  │  USB −55 dB"`.
    pub fn tick_text(&mut self, offset_db: f32) -> String {
        if self.items.is_empty() {
            return "No mic".to_owned();
        }
        self.items
            .iter_mut()
            .map(|readout| match readout.tick() {
                Some(db) => format!("{} {} dB", readout.label, format_db(db + offset_db)),
                None => format!("{} n/a", readout.label),
            })
            .collect::<Vec<_>>()
            .join(SEPARATOR)
    }
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
