//! Measurement/instrumentation scaffolding, compiled only under the
//! `instrument` feature. Product builds contain none of this code, so the
//! shipping binary carries no CSV output, synthetic input, or development
//! operations. All `HANE_*` environment variables are interpreted here in a
//! single place.

use hane_editor::InputMeasurement;
use hane_metrics::{DurationDistribution, FrameMetrics};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A single interpretation of the `HANE_*` measurement environment variables.
///
/// Both the UI (CSV output) and the app harness (synthetic input) read the
/// same configuration, so variable names live in exactly one function.
#[derive(Clone, Debug)]
pub struct InstrumentationConfig {
    pub metrics_csv: Option<PathBuf>,
    pub scenario: String,
    pub input_source: String,
    pub refresh_rate_hz: String,
    pub gate: Option<PathBuf>,
    pub start_empty: bool,
    pub no_focus: bool,
    pub measurement_cursor_offset: Option<usize>,
    pub dev_cursor_down: Option<usize>,
    pub autoscroll: bool,
    pub measure_idle_rss: bool,
    pub background_presentation: bool,
}

impl InstrumentationConfig {
    pub fn from_environment() -> Self {
        fn flag(name: &str) -> bool {
            std::env::var(name).is_ok_and(|value| !value.is_empty())
        }
        fn usize_var(name: &str) -> Option<usize> {
            std::env::var(name)
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| {
                    value
                        .parse::<usize>()
                        .unwrap_or_else(|_| panic!("{name} must be a non-negative integer"))
                })
        }
        Self {
            metrics_csv: std::env::var_os("HANE_METRICS_CSV").map(PathBuf::from),
            scenario: std::env::var("HANE_METRICS_SCENARIO")
                .unwrap_or_else(|_| "unspecified".into()),
            input_source: std::env::var("HANE_INPUT_SOURCE")
                .unwrap_or_else(|_| "unspecified".into()),
            refresh_rate_hz: std::env::var("HANE_REFRESH_RATE_HZ")
                .unwrap_or_else(|_| "unknown".into()),
            gate: std::env::var_os("HANE_METRICS_GATE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            start_empty: flag("HANE_MEASUREMENT_EMPTY"),
            no_focus: flag("HANE_NO_FOCUS"),
            measurement_cursor_offset: usize_var("HANE_MEASUREMENT_CURSOR_OFFSET"),
            dev_cursor_down: usize_var("HANE_DEV_CURSOR_DOWN"),
            autoscroll: flag("HANE_AUTOSCROLL"),
            measure_idle_rss: flag("HANE_MEASURE_IDLE_RSS"),
            background_presentation: flag("HANE_BACKGROUND_PRESENTATION"),
        }
    }
}

/// Runtime measurement state carried by `EditorView` only in instrument builds.
pub(crate) struct Instrumentation {
    pub(crate) metrics_output: Option<Phase0MetricsOutput>,
    pub(crate) process_started: Instant,
    pub(crate) file_open_time: Duration,
    pub(crate) load_rss_bytes: Option<u64>,
    pub(crate) ready_reported: bool,
    pub(crate) ready_armed: bool,
    pub(crate) display_linked_scroll_direction: Option<f32>,
}

impl Instrumentation {
    pub(crate) fn from_environment() -> Self {
        let config = InstrumentationConfig::from_environment();
        let metrics_output = Phase0MetricsOutput::new(&config).unwrap_or_else(|error| {
            eprintln!("could not open HANE_METRICS_CSV: {error}");
            None
        });
        Self {
            metrics_output,
            process_started: Instant::now(),
            file_open_time: Duration::ZERO,
            load_rss_bytes: None,
            ready_reported: false,
            ready_armed: false,
            display_linked_scroll_direction: None,
        }
    }
}

pub(crate) struct Phase0MetricsOutput {
    file: File,
    scenario: String,
    input_source: String,
    refresh_rate_hz: String,
    background_job: bool,
    gate: Option<PathBuf>,
}

impl Phase0MetricsOutput {
    pub(crate) fn new(config: &InstrumentationConfig) -> io::Result<Option<Self>> {
        let Some(path) = config.metrics_csv.as_deref() else {
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
            scenario: config.scenario.clone(),
            input_source: config.input_source.clone(),
            refresh_rate_hz: config.refresh_rate_hz.clone(),
            background_job: config.background_presentation,
            gate: config.gate.clone(),
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
