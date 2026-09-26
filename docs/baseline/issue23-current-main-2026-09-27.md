# Hane current-main startup and RSS baseline

Issue: [#23](https://github.com/hide212131/hane/issues/23)

Current `main` at report preparation: `b4061b9d43cd0c14dc4894cc4e27fa9227bd8199`

Measured product source: `174558e522feb7ed17e9cc7567dcd71e0499a7d6`. The intervening `main` changes are documentation and agent instructions only; product code is unchanged.

Capture date: 2026-09-26 and 2026-09-27 (JST)

This records the post-refactor baseline for the product code on current `main`. It is a measurement, not an
optimization. The `instrument` feature was used to capture readiness, file-load, RSS and
work-folder completion. The measurement helpers added for this run are feature-gated; the
ordinary release build does not include them. No shipping-path performance change was made.

## Headline results

The compact current headline is in [`README.md`](README.md). Startup was captured in two
independent runs of 30 process launches each. Memory values are per-process resident set size
(RSS), in bytes, after the listed idle period.

| Scenario | Samples | Median | p95 | Notes |
|---|---:|---:|---:|---|
| Empty warm startup | 60 | 187.183 ms | 227.768 ms | 30 samples per independent run |
| Empty unpurged startup | 60 | 185.172 ms | 199.390 ms | OS cache was not purged; not a true cold result |
| 1 KiB Markdown startup | 60 | 196.986 ms | 227.932 ms | File-open median 70.992 ms |
| 100 MiB Markdown startup | 60 | 372.127 ms | 390.542 ms | File-open median 235.174 ms |
| Empty work-folder startup | 60 | 188.444 ms | 214.452 ms | Scan completion median 307.835 ms |
| 1k-note work-folder startup | 60 | 189.263 ms | 210.459 ms | Scan completion median 312.885 ms |
| 10k-note work-folder startup | 60 | 187.074 ms | 204.975 ms | Scan completion median 646.099 ms |

The first editor paint for a 10k-note folder is ready at a median of 187.074 ms while the
scan completes at a median of 646.099 ms (p95 681.726 ms). Folder discovery therefore
finished after the editor became ready in these runs.

| RSS scenario | Samples | Measured bytes | MB (decimal) | MiB (binary) | Initial budget |
|---|---:|---:|---:|---:|---:|
| Empty editor, 30 s idle | 2 | 97,533,952–98,467,840 | 97.5–98.5 | 93.0–93.9 | <65 MiB |
| 1 MiB document, 30 s idle | 2 | 121,995,264–122,175,488 | 122.0–122.2 | 116.3–116.5 | — |
| 10 MiB document, 30 s idle | 2 | 317,440,000–371,064,832 | 317.4–371.1 | 302.7–353.9 | <120 MB |
| 100 MiB document, 30 s idle | 2 | 1,298,186,240–1,366,327,296 | 1,298.2–1,366.3 | 1,238.0–1,303.0 | <350 MB |
| Empty work folder, 30 s after scan | 2 | 76,595,200–87,818,240 | 76.6–87.8 | 73.1–83.7 | — |
| 1k-note folder, 30 s after scan | 2 | 124,141,568–126,615,552 | 124.1–126.6 | 118.4–120.8 | — |
| 10k-note folder, 30 s after scan | 2 | 405,569,536–406,618,112 | 405.6–406.6 | 386.8–387.8 | — |
| 10 visited notes, then 30 s idle | 1 | 133,529,600 | 133.5 | 127.4 | — |
| 100 visited notes, then 30 s idle | 1 | 143,245,312 | 143.2 | 136.6 | — |
| 1,000 visited notes, then 30 s idle | 1 | 258,473,984 | 258.5 | 246.5 | — |
| Open 100 MiB note, return to small note, then 30 s idle | 1 | 355,090,432 | 355.1 | 338.6 | — |

Folder RSS measurements include the first note that work-folder mode opens automatically;
the 1k/10k runs did not open the remaining indexed notes. Visiting notes opened and waited
for each note's file load to finish before continuing. Session counts at the three checkpoints
were 10, 100 and 1,000.

The 20-cycle long-running scenario switched between two work folders, opened two notes per
folder (including an image note), and performed 10 edit/undo/redo cycles per folder switch.
RSS was 99.1 MB after cycle 1, 109.7 MB after cycle 5, 109.7 MB after cycle 10, 109.8 MB
after cycle 20, and 109.2 MB after a final 30-second idle period. This one run plateaued
after the first few cycles; it does not establish behavior for arbitrarily long sessions.

## Budget and comparison assessment

- The ≤150 ms warm-start target was exceeded in both independent runs: the run medians were
  186.464 ms and 189.001 ms. This is a repeated absolute-gate failure.
- The empty-editor, 10 MiB and 100 MiB RSS gates were exceeded in both independent samples.
  The 10 MiB readings varied by about 17%, but both were well over the 120 MB limit.
- A true cold-start result is unknown. `/usr/sbin/purge` returned nonzero, so the script
  labelled those samples “OS cache not purged.” The 400 ms cold gate cannot be marked pass.
- The previous R0 reference is 165.934 ms warm startup, 103.6 MB for a 10 MB document and
  263.1 MB for a 100 MB document. Those values were captured on macOS 26.5.1 with Rust 1.93.1
  and GPUI 0.2.2; this run used macOS 26.6.2, Rust 1.98.1 and GPUI-pre 0.3.6. Although the
  hardware family is the same, the 10% relative-regression rule is not a valid conclusion
  across these changed conditions. The current absolute-gate failures are still recorded.
- No optimization was performed. The existing evidence-led performance work is tracked by
  [#314](https://github.com/hide212131/hane/issues/314), subject to that issue's stated
  prerequisites.

## Machine and build conditions

| Item | Value |
|---|---|
| Hardware | MacBook Pro Mac15,7; Apple M3 Pro; 12 cores; 36 GB RAM |
| OS | macOS 26.6.2, build 25G83 (Darwin 25.6.0) |
| Display | 3456 × 2234 internal display; variable refresh; `CGDisplayMode` reported 0 Hz |
| Power | AC attached; battery 80%, not charging; Low Power Mode off |
| Rust | `rustc 1.98.1 (48a229cea 2026-09-01)` from the repository toolchain pin |
| GPUI | `gpui-pre 0.3.6` |
| Build | `release`, feature `instrument` |
| Input source | `com.apple.keylayout.ABC` |

The machine had one separately launched, idle Hane process from another checkout at the
start of the work. Its per-process RSS is not included in any measurement here. The app
measurement reports the measured process's own `mach_task_basic_info.resident_size`.

## Fixtures and measurement definitions

The existing `hane-bench fixtures` command generated the large documents with exact sizes
1,048,576 bytes (1 MiB), 10,485,760 bytes (10 MiB), and 104,857,600 bytes (100 MiB). The
issue-specific generator creates:

- `small.md`: 1,024 bytes.
- An empty folder, a folder of 1,000 Markdown files, and a folder of 10,000 Markdown files.
  Each generated note is exactly 1,024 bytes, with fixed-width filenames; note contents
  repeat deterministically from the fixture number and fixed byte pattern.
- A two-note folder containing the 100 MiB fixture and a 1 KiB note.
- Two two-note switch folders. Each has a Markdown image note and a text note; the image is
  copied from `assets/phase4-feather.svg`.

The fixtures are disposable under `target/fixtures/issue23`; the large inputs are not checked
into Git. Rebuild them with:

```sh
cargo +1.98.1 run --locked --release -p hane-benchmark --bin hane-bench -- fixtures
python3 scripts/prepare_issue23_fixtures.py
python3 scripts/prepare_issue23_fixtures.py --verify
cargo +1.98.1 build --locked --release -p hane --features instrument
```

Run the full startup and RSS capture with a shared output directory:

```sh
HANE_MEASUREMENT_SAMPLES=30 scripts/measure.sh startup target/measure/issue23-current-main
scripts/measure.sh memory target/measure/issue23-current-main
```

The memory command defaults to two separate process trials for empty, 1/10/100 MiB documents
and empty/1k/10k folders. It also records the 10/100/1,000-note checkpoints, the large-to-
small return scenario, and the 20-cycle long-running scenario. Its longer visit and long-run
sections use a 30-second idle period by default. `HANE_MEASUREMENT_SAMPLES` controls startup
sample count; `HANE_MEASUREMENT_MEMORY_REPEATS` controls the independent memory trials.

The script attempts `/usr/sbin/purge` before each cold-labelled sample. A nonzero exit is
recorded as “OS cache not purged”; these are separate-process launches with warm OS/file
caches, not cold-cache measurements. Warm runs also use a new app process for every sample
without purging OS caches. Startup is measured from process start to the first `InputCapture`
paint. Work-folder completion is logged separately after asynchronous scan and index
publication; the automatically opened first note may still be loading then.

RSS is the operating system's resident-set byte count, not virtual memory. In this report,
`MB = 1,000,000 bytes` and `MiB = 1,048,576 bytes`; raw CSV always retains exact bytes.
Document RSS is sampled 30 seconds after instrumentation is armed just after window creation.
Work-folder RSS is
sampled 30 seconds after scan completion. Visit and long-run checkpoints wait 30 seconds
after each checkpoint before recording RSS.

The aggregator uses nearest-rank percentiles: sort the samples and select index
`ceil(n × p) - 1` for percentile `p`. The full per-record CSV, exact app event log and
aggregated table are linked below.

## Raw data and validation

- [`raw/measurements.csv`](issue23-current-main-2026-09-27/raw/measurements.csv) contains all
  UI metric rows with their source trial path.
- [`raw/measurement_events.log`](issue23-current-main-2026-09-27/raw/measurement_events.log)
  preserves the app's readiness, scan completion, session count and RSS event lines.
- [`raw/aggregated-results.md`](issue23-current-main-2026-09-27/raw/aggregated-results.md)
  is the direct output of `scripts/aggregate_metrics.py` for the collected run set.

Harness checks and workspace validation on Rust 1.98.1:

- `sh -n scripts/measure.sh`: passed.
- Fixture generation followed by `--verify`: passed.
- `cargo test --workspace --all-features`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- `cargo test --workspace --locked`: passed.
- `cargo build --release --locked -p hane`: passed.

Only this Mac configuration was measured. Windows, Linux, other Mac hardware, and a
successfully purged cold-cache state remain unmeasured. The merge can be reverted as a
single PR revert; no product optimization is included.
