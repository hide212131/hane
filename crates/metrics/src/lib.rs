//! Small, dependency-free rolling metrics primitives shared by the UI and benchmarks.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct RollingWindow<T> {
    capacity: usize,
    values: VecDeque<T>,
}

impl<T> RollingWindow<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "rolling-window capacity must be non-zero");
        Self {
            capacity,
            values: VecDeque::with_capacity(capacity),
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
    frame_intervals: RollingWindow<Duration>,
    layout_latencies: RollingWindow<Duration>,
    last_paint_at: Option<Instant>,
}

impl FrameMetrics {
    pub fn new(capacity: usize) -> Self {
        Self {
            painted_latencies: RollingWindow::new(capacity),
            frame_intervals: RollingWindow::new(capacity),
            layout_latencies: RollingWindow::new(capacity),
            last_paint_at: None,
        }
    }

    pub fn record_paint(&mut self, at: Instant, latencies: impl IntoIterator<Item = Duration>) {
        if let Some(previous) = self.last_paint_at.replace(at) {
            self.frame_intervals.push(at.duration_since(previous));
        }
        self.painted_latencies.extend(latencies);
    }

    pub fn record_layout(&mut self, latency: Duration) {
        self.layout_latencies.push(latency);
    }

    pub fn painted_percentile(&self, percentile: f64) -> Option<Duration> {
        self.painted_latencies.percentile(percentile)
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
}
