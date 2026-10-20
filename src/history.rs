//! In-memory ring buffer of recent levels for one device (never written to disk).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How much history is kept and charted.
pub const SPAN: Duration = Duration::from_secs(300);
/// Minimum spacing between stored samples (2 Hz → at most 600 points per device).
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

/// One stored point; `None` marks a gap (device not capturing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub at: Instant,
    pub db: Option<f32>,
}

#[derive(Debug, Default)]
pub struct History {
    samples: VecDeque<Sample>,
}

impl History {
    /// Appends a sample unless the previous one is more recent than [`SAMPLE_INTERVAL`],
    /// then drops samples older than [`SPAN`].
    pub fn record(&mut self, at: Instant, db: Option<f32>) {
        let due = self
            .samples
            .back()
            .is_none_or(|last| at.saturating_duration_since(last.at) >= SAMPLE_INTERVAL);
        if due {
            self.samples.push_back(Sample { at, db });
        }
        while self
            .samples
            .front()
            .is_some_and(|first| at.saturating_duration_since(first.at) > SPAN)
        {
            self.samples.pop_front();
        }
    }

    pub fn samples(&self) -> impl Iterator<Item = &Sample> {
        self.samples.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttles_to_interval_and_expires_after_span() {
        let start = Instant::now();
        let mut history = History::default();
        for tenth in 0..=3100u64 {
            history.record(start + Duration::from_millis(tenth * 100), Some(-40.0));
        }
        let samples: Vec<_> = history.samples().collect();
        assert_eq!(samples.len(), 601);
        let oldest = samples.first().expect("non-empty").at;
        assert_eq!(oldest, start + Duration::from_secs(10));
    }
}
