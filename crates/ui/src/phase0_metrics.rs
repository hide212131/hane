use hane_editor::InputMeasurement;
use hane_metrics::{DurationDistribution, FrameMetrics};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

pub(crate) struct Phase0MetricsOutput {
    file: File,
    scenario: String,
    input_source: String,
    refresh_rate_hz: String,
    background_job: bool,
    gate: Option<PathBuf>,
}

impl Phase0MetricsOutput {
    pub(crate) fn from_environment() -> io::Result<Option<Self>> {
        let Some(path) = std::env::var_os("HANE_METRICS_CSV").map(PathBuf::from) else {
            return Ok(None);
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        writeln!(
            file,
            "record_type,scenario,sequence,input_event_kind,keystroke_to_model_ms,keystroke_to_frame_ms,frame_interval_ms,layout_ms,startup_ms,file_open_ms,rss_bytes,input_source,refresh_rate_hz,background_job"
        )?;
        Ok(Some(Self {
            file,
            scenario: std::env::var("HANE_METRICS_SCENARIO")
                .unwrap_or_else(|_| "unspecified".into()),
            input_source: std::env::var("HANE_INPUT_SOURCE")
                .unwrap_or_else(|_| "unspecified".into()),
            refresh_rate_hz: std::env::var("HANE_REFRESH_RATE_HZ")
                .unwrap_or_else(|_| "unknown".into()),
            background_job: std::env::var("HANE_PHASE0_BACKGROUND_PRESENTATION")
                .is_ok_and(|value| !value.is_empty()),
            gate: std::env::var_os("HANE_METRICS_GATE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        }))
    }

    pub(crate) fn ready(
        &mut self,
        startup: Duration,
        file_open: Duration,
        rss_bytes: Option<u64>,
    ) -> io::Result<()> {
        self.row(
            "ready",
            None,
            "",
            None,
            None,
            None,
            None,
            Some(startup),
            Some(file_open),
            rss_bytes,
        )
    }

    pub(crate) fn memory(&mut self, record_type: &str, rss_bytes: Option<u64>) -> io::Result<()> {
        self.row(
            record_type,
            None,
            "",
            None,
            None,
            None,
            None,
            None,
            None,
            rss_bytes,
        )
    }

    pub(crate) fn paint(
        &mut self,
        interval: Option<Duration>,
        layout: Option<Duration>,
    ) -> io::Result<()> {
        if !self.recording() {
            return Ok(());
        }
        self.row(
            "paint", None, "", None, None, interval, layout, None, None, None,
        )
    }

    pub(crate) fn input(&mut self, measurement: &InputMeasurement) -> io::Result<()> {
        if !self.recording() {
            return Ok(());
        }
        self.row(
            "input",
            Some(measurement.sequence),
            measurement.kind.as_str(),
            Some(measurement.keystroke_to_model()),
            measurement.keystroke_to_frame(),
            None,
            None,
            None,
            None,
            None,
        )
    }

    fn recording(&self) -> bool {
        self.gate.as_ref().is_none_or(|path| path.exists())
    }

    #[allow(clippy::too_many_arguments)]
    fn row(
        &mut self,
        record_type: &str,
        sequence: Option<u64>,
        input_event_kind: &str,
        model: Option<Duration>,
        frame: Option<Duration>,
        interval: Option<Duration>,
        layout: Option<Duration>,
        startup: Option<Duration>,
        file_open: Option<Duration>,
        rss_bytes: Option<u64>,
    ) -> io::Result<()> {
        fn milliseconds(value: Option<Duration>) -> String {
            value.map_or_else(String::new, |value| {
                format!("{:.6}", value.as_secs_f64() * 1_000.0)
            })
        }
        writeln!(
            self.file,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv(record_type),
            csv(&self.scenario),
            sequence.map_or_else(String::new, |value| value.to_string()),
            csv(input_event_kind),
            milliseconds(model),
            milliseconds(frame),
            milliseconds(interval),
            milliseconds(layout),
            milliseconds(startup),
            milliseconds(file_open),
            rss_bytes.map_or_else(String::new, |value| value.to_string()),
            csv(&self.input_source),
            csv(&self.refresh_rate_hz),
            self.background_job,
        )?;
        self.file.flush()
    }
}

fn csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(crate) fn log_summary(metrics: &FrameMetrics) {
    fn fields(name: &str, distribution: DurationDistribution) -> String {
        format!(
            "{name}_samples={} {name}_median_ms={:.3} {name}_p95_ms={:.3} {name}_p99_ms={:.3} {name}_max_ms={:.3}",
            distribution.samples,
            distribution.median.as_secs_f64() * 1_000.0,
            distribution.p95.as_secs_f64() * 1_000.0,
            distribution.p99.as_secs_f64() * 1_000.0,
            distribution.max.as_secs_f64() * 1_000.0,
        )
    }
    eprintln!(
        "hane_metrics {} {} {} {}",
        fields(
            "keystroke_to_model",
            metrics.keystroke_to_model_distribution()
        ),
        fields(
            "keystroke_to_frame",
            metrics.keystroke_to_frame_distribution()
        ),
        fields("frame_interval", metrics.frame_interval_distribution()),
        fields("layout", metrics.layout_distribution()),
    );
}
