//! Debounced "level above threshold" indicator.

use std::time::{Duration, Instant};

/// The level must stay above the threshold this long before the alarm turns on,
/// and below it this long before it turns off, so coughs and bumps don't flash it.
pub const SUSTAIN: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub struct Alarm {
    active: bool,
    /// Start of the current run on the opposite side of the threshold from `active`.
    pending_since: Option<Instant>,
}

impl Alarm {
    /// Feeds one observation and returns whether the alarm is on.
    pub fn update(&mut self, now: Instant, above: bool) -> bool {
        if above == self.active {
            self.pending_since = None;
        } else {
            let since = *self.pending_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= SUSTAIN {
                self.active = above;
                self.pending_since = None;
            }
        }
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_sustained_level_to_switch_on_and_off() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut alarm = Alarm::default();
        assert!(!alarm.update(at(0), true));
        assert!(!alarm.update(at(500), false), "short burst is ignored");
        assert!(!alarm.update(at(600), true));
        assert!(alarm.update(at(1600), true), "on after 1 s above");
        assert!(alarm.update(at(1700), false), "stays on while released");
        assert!(
            alarm.update(at(2000), true),
            "re-rising resets the release timer"
        );
        assert!(alarm.update(at(2100), false));
        assert!(!alarm.update(at(3100), false), "off after 1 s below");
    }
}
