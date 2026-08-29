//! Runtime timing, rolling-window, distribution, and process-memory primitives.

// Sampling/query methods are intentionally usable as discardable instrumentation
// probes, so forcing every caller to bind their result is not useful.
#![allow(
    clippy::must_use_candidate,
    reason = "instrumentation queries may intentionally discard sampled values"
)]
// Percentile conversion is range-checked by its caller; the float-to-index
// conversion is the documented nearest-rank calculation.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "percentile calculation intentionally converts bounded floating-point ranks to indices"
)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[allow(deprecated)]
pub fn process_memory_bytes() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::uninit();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    // SAFETY: task_info initializes `info` for the current task when it returns KERN_SUCCESS.
    let status = unsafe {
        libc::task_info(
            libc::mach_task_self_,
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &raw mut count,
        )
    };
    if status != libc::KERN_SUCCESS {
        return None;
    }
    // SAFETY: a successful task_info call initialized all MACH_TASK_BASIC_INFO fields.
    Some(unsafe { info.assume_init() }.resident_size)
}

#[cfg(not(target_os = "macos"))]
pub fn process_memory_bytes() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .ok()
        .map(|kilobytes| kilobytes * 1_024)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DurationDistribution {
    pub samples: usize,
    pub median: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub max: Duration,
}

pub fn duration_distribution(values: impl IntoIterator<Item = Duration>) -> DurationDistribution {
    let mut sorted: Vec<_> = values.into_iter().collect();
    if sorted.is_empty() {
        return DurationDistribution::default();
    }
    sorted.sort_unstable();
    DurationDistribution {
        samples: sorted.len(),
        median: percentile(sorted.iter().copied(), 0.50).unwrap_or_default(),
        p95: percentile(sorted.iter().copied(), 0.95).unwrap_or_default(),
        p99: percentile(sorted.iter().copied(), 0.99).unwrap_or_default(),
        max: sorted.last().copied().unwrap_or_default(),
    }
}

#[derive(Clone, Debug)]
pub struct RollingWindow<T> {
    capacity: usize,
    values: VecDeque<T>,
}

impl<T> RollingWindow<T> {
    /// Creates a rolling window with the specified capacity.
    ///
    /// # Panics
    ///
    /// Panics when `capacity` is zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "rolling-window capacity must be non-zero");
        Self {
            capacity,
            // Keep the logical bound without reserving the worst case in every
            // fresh editor. Metrics grow only when samples actually arrive.
            values: VecDeque::new(),
        }
    }

    pub fn push(&mut self, value: T) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }

    pub fn extend(&mut self, values: impl IntoIterator<Item = T>) {
        for value in values {
            self.push(value);
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.values.iter()
    }

    pub fn back(&self) -> Option<&T> {
        self.values.back()
    }

    pub fn take_all(&mut self) -> Vec<T> {
        self.values.drain(..).collect()
    }
}

impl<T: Copy + Ord> RollingWindow<T> {
    pub fn percentile(&self, quantile: f64) -> Option<T> {
        percentile(self.values.iter().copied(), quantile)
    }
}

pub fn percentile<T: Ord>(values: impl IntoIterator<Item = T>, percentile: f64) -> Option<T> {
    let mut sorted: Vec<_> = values.into_iter().collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_unstable();
    let percentile = percentile.clamp(0.0, 1.0);
    let index = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    Some(sorted.remove(index))
}

#[derive(Clone, Debug)]
pub struct FrameMetrics {
    painted_latencies: RollingWindow<Duration>,
    model_latencies: RollingWindow<Duration>,
    frame_intervals: RollingWindow<Duration>,
    layout_latencies: RollingWindow<Duration>,
    last_paint_at: Option<Instant>,
}

impl FrameMetrics {
    pub fn new(capacity: usize) -> Self {
        Self {
            painted_latencies: RollingWindow::new(capacity),
            model_latencies: RollingWindow::new(capacity),
            frame_intervals: RollingWindow::new(capacity),
            layout_latencies: RollingWindow::new(capacity),
            last_paint_at: None,
        }
    }

    pub fn record_paint(
        &mut self,
        at: Instant,
        model_latencies: impl IntoIterator<Item = Duration>,
        frame_latencies: impl IntoIterator<Item = Duration>,
    ) -> Option<Duration> {
        let interval = self
            .last_paint_at
            .replace(at)
            .map(|previous| at.duration_since(previous));
        if let Some(interval) = interval {
            self.frame_intervals.push(interval);
        }
        self.model_latencies.extend(model_latencies);
        self.painted_latencies.extend(frame_latencies);
        interval
    }

    pub fn record_layout(&mut self, latency: Duration) {
        self.layout_latencies.push(latency);
    }

    pub fn latest_layout(&self) -> Option<Duration> {
        self.layout_latencies.back().copied()
    }

    pub fn painted_percentile(&self, percentile: f64) -> Option<Duration> {
        self.painted_latencies.percentile(percentile)
    }

    pub fn keystroke_to_model_distribution(&self) -> DurationDistribution {
        duration_distribution(self.model_latencies.iter().copied())
    }

    pub fn keystroke_to_frame_distribution(&self) -> DurationDistribution {
        duration_distribution(self.painted_latencies.iter().copied())
    }

    pub fn frame_interval_distribution(&self) -> DurationDistribution {
        duration_distribution(self.frame_intervals.iter().copied())
    }

    pub fn layout_distribution(&self) -> DurationDistribution {
        duration_distribution(self.layout_latencies.iter().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_window_evicts_oldest_sample() {
        let mut values = RollingWindow::new(3);
        values.extend([1, 2, 3, 4]);
        assert_eq!(values.iter().copied().collect::<Vec<_>>(), [2, 3, 4]);
        assert_eq!(values.percentile(0.5), Some(3));
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        assert_eq!(percentile(1..=100, 0.95), Some(95));
        assert_eq!(percentile(Vec::<u8>::new(), 0.95), None);
    }

    #[test]
    fn duration_distribution_reports_all_required_statistics() {
        let distribution = duration_distribution((1..=100).map(Duration::from_millis));
        assert_eq!(distribution.samples, 100);
        assert_eq!(distribution.median, Duration::from_millis(50));
        assert_eq!(distribution.p95, Duration::from_millis(95));
        assert_eq!(distribution.p99, Duration::from_millis(99));
        assert_eq!(distribution.max, Duration::from_millis(100));
    }
}
