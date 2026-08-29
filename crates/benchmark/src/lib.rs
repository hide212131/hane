//! Offline fixtures, benchmark scenarios, environment capture, and report rendering.

// Scenario/report helpers prioritize readable fixtures and deterministic reporting;
// their short-lived values and formatting are intentionally not production APIs.
#![allow(
    clippy::pedantic,
    reason = "offline benchmark fixtures intentionally favor legible deterministic scenario construction"
)]

use hane_document::{LineId, RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::BlockIndex;
use hane_metrics::{DurationDistribution, duration_distribution};
use hane_presentation::testing::FixedAdvanceShaper;
use hane_presentation::{
    BlockLine, BlockWindow, HeightIndex, block_heights, block_line_span, layout_block,
    present_block, present_markdown, trailing_blank_lines,
};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub type Distribution = DurationDistribution;

pub fn distribution(samples: &[Duration]) -> Distribution {
    duration_distribution(samples.iter().copied())
}

#[derive(Clone, Debug)]
pub struct Environment {
    pub git_commit: String,
    pub profile: String,
    pub rustc: String,
    pub gpui: String,
    pub os: String,
    pub cpu: String,
    pub memory_bytes: Option<u64>,
    pub refresh_rate_hz: Option<f32>,
}

impl Environment {
    pub fn collect(profile: impl Into<String>) -> Self {
        fn output(program: &str, args: &[&str]) -> String {
            Command::new(program)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_else(|| "unknown".into())
        }
        let memory_bytes = hane_metrics::process_memory_bytes();
        Self {
            git_commit: output("git", &["rev-parse", "HEAD"]),
            profile: profile.into(),
            rustc: output("rustc", &["--version"]),
            gpui: "0.2.2".into(),
            os: output("sw_vers", &["-productVersion"]),
            cpu: output("sysctl", &["-n", "machdep.cpu.brand_string"]),
            memory_bytes,
            refresh_rate_hz: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Fixture {
    pub name: &'static str,
    pub target_bytes: usize,
    pub pattern: &'static str,
}

pub const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "markdown_1mb.md",
        target_bytes: 1024 * 1024,
        pattern: "# 見出し\n\nこれは **重要な段落** です。ASCII, 日本語, emoji 🙂, e\u{301}.\n\n",
    },
    Fixture {
        name: "markdown_10mb.md",
        target_bytes: 10 * 1024 * 1024,
        pattern: "# 見出し\n\nこれは **重要な段落** です。ASCII, 日本語, emoji 🙂, e\u{301}.\n\n",
    },
    Fixture {
        name: "markdown_100mb.md",
        target_bytes: 100 * 1024 * 1024,
        pattern: "## Large document\n\nThe quick brown fox. **bold text** 日本語 羽 🙂 e\u{301}.\n\n",
    },
    Fixture {
        name: "paragraphs_100k.md",
        target_bytes: 3_100_000,
        pattern: "短い段落です。 **bold**\n",
    },
    Fixture {
        name: "japanese.md",
        target_bytes: 1024 * 1024,
        pattern: "日本語入力の検証用テキストです。変換・確定・取消を確認します。\n",
    },
    Fixture {
        name: "unicode_mixed.md",
        target_bytes: 1024 * 1024,
        pattern: "ASCII 日本語 🙂 👨‍👩‍👧‍👦 e\u{301} 𠮷野家 **混在**\r\n",
    },
];

pub fn generate_fixtures(root: &Path) -> io::Result<Vec<PathBuf>> {
    fs::create_dir_all(root)?;
    let mut paths = Vec::new();
    for fixture in FIXTURES {
        let path = root.join(fixture.name);
        let file = File::create(&path)?;
        let mut writer = BufWriter::new(file);
        let complete_patterns = fixture.target_bytes / fixture.pattern.len();
        for _ in 0..complete_patterns {
            writer.write_all(fixture.pattern.as_bytes())?;
        }
        for _ in 0..(fixture.target_bytes % fixture.pattern.len()) {
            writer.write_all(b"#")?;
        }
        writer.flush()?;
        paths.push(path);
    }
    Ok(paths)
}

pub fn run_buffer_edit_scenario(bytes: usize, iterations: usize) -> Distribution {
    let pattern = "paragraph **bold** 日本語 🙂\n";
    let mut source = String::with_capacity(bytes);
    while source.len() < bytes {
        source.push_str(pattern);
    }
    source.truncate(
        (0..=bytes.min(source.len()))
            .rev()
            .find(|&i| source.is_char_boundary(i))
            .unwrap(),
    );
    let mut buffer = RopeBuffer::from_text(&source);
    let mut samples = Vec::with_capacity(iterations);
    for ix in 0..iterations {
        let positions = [0, buffer.len_bytes().0 / 2, buffer.len_bytes().0];
        let raw = positions[ix % positions.len()];
        let offset = (0..=raw)
            .rev()
            .find(|&i| {
                buffer
                    .validate_offset(hane_document::SourceOffset(i))
                    .is_ok()
            })
            .unwrap();
        let start = std::time::Instant::now();
        let summary = buffer.edit(SourceRange::empty(offset), "羽").unwrap();
        samples.push(start.elapsed());
        buffer.edit(summary.range_after, "").unwrap();
    }
    distribution(&samples)
}

pub fn run_file_open_scenario(path: &Path, iterations: usize) -> io::Result<Distribution> {
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = std::time::Instant::now();
        let file = File::open(path)?;
        let buffer = RopeBuffer::from_reader(std::io::BufReader::new(file))?;
        std::hint::black_box(buffer.len_bytes());
        samples.push(start.elapsed());
    }
    Ok(distribution(&samples))
}

pub fn run_presentation_scenario(iterations: usize) -> Distribution {
    let source = "これは **重要な日本語🙂** を含む active block です。";
    let range = SourceRange::new(0, source.len());
    let mut samples = Vec::with_capacity(iterations);
    for revision in 0..iterations {
        let start = std::time::Instant::now();
        let block = present_markdown(
            revision as u64,
            hane_document::Revision(revision as u64),
            range,
            source,
            26.0,
        );
        std::hint::black_box(block);
        samples.push(start.elapsed());
    }
    distribution(&samples)
}

pub fn run_layout_scenario(blocks: usize, iterations: usize) -> Distribution {
    let mut heights = HeightIndex::new((0..blocks).map(|index| 20.0 + (index % 7) as f32));
    let mut samples = Vec::with_capacity(iterations);
    for iteration in 0..iterations {
        let start = std::time::Instant::now();
        let index = iteration % blocks;
        heights.update(index, 22.0 + (iteration % 5) as f32);
        let visible = heights.visible_range((iteration * 137) as f32, 720.0, 260.0);
        std::hint::black_box(visible);
        samples.push(start.elapsed());
    }
    distribution(&samples)
}

/// Presenting and laying out the blocks of one viewport, the work R4B added to
/// each frame: rows are built for the visible lines of every visible block, and
/// their wrap points and heights decided. Text measurement is the fixed-advance
/// stand-in, so this is the layout cost without the font behind it.
pub fn run_block_layout_scenario(blocks: usize, iterations: usize) -> Distribution {
    let mut source = String::new();
    for paragraph in 0..blocks {
        source.push_str(&format!(
            "paragraph {paragraph} with enough words in it to wrap onto a second row\n\n"
        ));
    }
    let buffer = RopeBuffer::from_text(&source);
    let index = BlockIndex::from_buffer(&buffer);
    let shaper = FixedAdvanceShaper::new(8.0);
    // A 720 px viewport of 26 px rows, in blocks of two lines each.
    let visible = 14;
    let mut samples = Vec::with_capacity(iterations);
    for iteration in 0..iterations {
        let first = (iteration * 13) % index.len().saturating_sub(visible).max(1);
        let start = std::time::Instant::now();
        for ordinal in first..first + visible {
            let Some(block) = index.block(ordinal) else {
                continue;
            };
            let Some(span) = block_line_span(&buffer, &block) else {
                continue;
            };
            let ranges = span
                .clone()
                .map(|line| buffer.line_range(LineId(line)).unwrap_or_default())
                .collect::<Vec<_>>();
            let texts = ranges
                .iter()
                .map(|range| buffer.text(*range).unwrap_or_default())
                .collect::<Vec<_>>();
            let lines = span
                .clone()
                .zip(&ranges)
                .zip(&texts)
                .map(|((line, range), text)| BlockLine {
                    line,
                    range: *range,
                    text,
                    disclosure: None,
                })
                .collect::<Vec<_>>();
            let visual = present_block(
                &block,
                buffer.revision(),
                &BlockWindow {
                    trailing_blank_lines: trailing_blank_lines(&buffer, &span),
                    span,
                    lines: &lines,
                },
                26.0,
            );
            std::hint::black_box(layout_block(&visual, 640.0, &shaper).height());
        }
        samples.push(start.elapsed());
    }
    distribution(&samples)
}

/// Building the block-driven height index for a document of `blocks`
/// paragraphs, as the background formal-index job does at publication time.
pub fn run_block_heights_scenario(blocks: usize, iterations: usize) -> Distribution {
    let mut source = String::new();
    for paragraph in 0..blocks {
        source.push_str(&format!("paragraph {paragraph} with a few words in it\n\n"));
    }
    let buffer = RopeBuffer::from_text(&source);
    let index = BlockIndex::from_buffer(&buffer);
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = std::time::Instant::now();
        let heights = HeightIndex::new(block_heights(&buffer, &index, 26.0));
        std::hint::black_box(heights.total_height());
        samples.push(start.elapsed());
    }
    distribution(&samples)
}

/// Alternating one block split/join in the middle of an existing height index.
/// The item count stays stable across each pair so every sample measures only
/// the structural edit, not construction of another document-sized index.
pub fn run_height_splice_scenario(blocks: usize, iterations: usize) -> Distribution {
    let mut heights = HeightIndex::new(std::iter::repeat_n(26.0, blocks));
    let middle = blocks / 2;
    let mut samples = Vec::with_capacity(iterations);
    for iteration in 0..iterations {
        let start = std::time::Instant::now();
        if iteration % 2 == 0 {
            heights.splice(middle..middle + 1, [26.0, 26.0]);
        } else {
            heights.splice(middle..middle + 2, [26.0]);
        }
        std::hint::black_box(heights.total_height());
        samples.push(start.elapsed());
    }
    distribution(&samples)
}

/// What one incremental `BlockIndex` update cost, beyond its duration.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BlockIndexMeasurement {
    pub update: Distribution,
    pub max_reparsed_bytes: usize,
    pub invalidated_blocks: usize,
}

/// Incremental block-index updates on a document of `blocks` paragraphs.
/// `structural` chooses between typing inside one block and edits that split a
/// block in two, which is the case that has to re-chunk the index.
pub fn run_block_index_scenario(
    blocks: usize,
    iterations: usize,
    structural: bool,
) -> BlockIndexMeasurement {
    let mut source = String::new();
    for paragraph in 0..blocks {
        source.push_str(&format!("paragraph {paragraph} with a few words in it\n\n"));
    }
    let mut buffer = RopeBuffer::from_text(&source);
    let mut index = BlockIndex::from_buffer(&buffer);
    let mut at = index
        .block(blocks / 2)
        .map_or(0, |block| block.source_range.start.0 + 10);
    let mut measurement = BlockIndexMeasurement::default();
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let base = buffer.revision();
        let insertion = if structural { "\n\n" } else { "x" };
        if buffer.edit(SourceRange::empty(at), insertion).is_err() {
            break;
        }
        let Ok(deltas) = buffer.deltas_since(base) else {
            break;
        };
        let start = std::time::Instant::now();
        let update = index.update(&buffer, &deltas);
        samples.push(start.elapsed());
        measurement.max_reparsed_bytes = measurement.max_reparsed_bytes.max(update.reparsed_bytes);
        measurement.invalidated_blocks += update.invalidated_blocks;
        at += insertion.len() + 1;
    }
    measurement.update = distribution(&samples);
    measurement
}

pub fn markdown_report(environment: &Environment, scenarios: &[(&str, Distribution)]) -> String {
    let mut report = format!(
        "# Hane Performance Report\n\n- Git: `{}`\n- Profile: `{}`\n- Rust: `{}`\n- GPUI: `{}`\n- OS: `{}`\n- CPU: `{}`\n- RSS: `{}` bytes\n\n| Scenario | Samples | Median (ms) | p95 (ms) | p99 (ms) | Max (ms) |\n|---|---:|---:|---:|---:|---:|\n",
        environment.git_commit,
        environment.profile,
        environment.rustc,
        environment.gpui,
        environment.os,
        environment.cpu,
        environment.memory_bytes.unwrap_or(0)
    );
    for (name, d) in scenarios {
        report.push_str(&format!(
            "| {name} | {} | {:.3} | {:.3} | {:.3} | {:.3} |\n",
            d.samples,
            d.median.as_secs_f64() * 1000.0,
            d.p95.as_secs_f64() * 1000.0,
            d.p99.as_secs_f64() * 1000.0,
            d.max.as_secs_f64() * 1000.0
        ));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percentiles_use_nearest_rank() {
        let d = distribution(&(1..=100).map(Duration::from_millis).collect::<Vec<_>>());
        assert_eq!(d.median, Duration::from_millis(50));
        assert_eq!(d.p95, Duration::from_millis(95));
        assert_eq!(d.p99, Duration::from_millis(99));
    }
}
