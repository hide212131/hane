# Hane current-main startup and RSS baseline

Issue: [#23](https://github.com/hide212131/hane/issues/23)

Current `main` at report preparation: `784707877d67323d38885d706ec55382bdf8dcdb`

Measured product source: `174558e522feb7ed17e9cc7567dcd71e0499a7d6`. The intervening `main` changes are documentation and agent instructions only; product code is unchanged.

Measurement harness revision: `58ab905db035e3d56ac4c59a8fbd6c2be0065f53`.

Capture date: 2026-09-27 (JST)

This records the post-refactor baseline for the product code on current `main`. It is a measurement, not an
optimization. The `instrument` feature was used to capture readiness, file-load, RSS and
work-folder completion. The measurement helpers added for this run are feature-gated; the
ordinary release build does not include them. No shipping-path performance change was made.

## Headline results

The compact current headline is in [`README.md`](README.md). Startup was captured in one full
confirmation run with 30 process launches per startup case. Document and work-folder RSS were
captured in two separate process trials per case. Visit and long-run checkpoints were captured
once each. RSS is per-process resident set size (RSS), in bytes, after the listed idle period.

| Scenario | Samples | Median | p95 | Notes |
|---|---:|---:|---:|---|
| Empty warm startup | 30 | 164.306 ms | 175.748 ms | New process per launch |
| Empty unpurged startup | 30 | 163.532 ms | 169.968 ms | OS cache was not purged; not a true cold result |
| 1 KiB Markdown startup | 30 | 171.249 ms | 183.681 ms | File-open median 60.512 ms |
| 100 MiB Markdown startup | 30 | 343.596 ms | 367.273 ms | File-open median 218.981 ms |
| Empty work-folder startup | 30 | 164.454 ms | 175.837 ms | Scan completion median 182.441 ms, p95 196.329 ms |
| 1k-note work-folder startup | 30 | 166.114 ms | 170.606 ms | Scan completion median 184.683 ms, p95 189.622 ms |
| 10k-note work-folder startup | 30 | 163.414 ms | 173.232 ms | Scan completion median 186.574 ms, p95 198.799 ms |

For 10k notes, the first editor paint was ready at a median of 163.414 ms while the scan
completed at a median of 186.574 ms (p95 198.799 ms). Folder discovery completed after the
editor became ready in these runs.

| RSS scenario | Samples | Measured bytes | MB (decimal) | MiB (binary) | Initial budget |
|---|---:|---:|---:|---:|---:|
| Empty editor, 30 s idle | 2 | 85,622,784–86,245,376 | 85.6–86.2 | 81.7–82.2 | <65 MiB |
| 1 MiB document, 30 s idle | 2 | 115,048,448–115,146,752 | 115.0–115.1 | 109.7–109.8 | — |
| 10 MiB document, 30 s idle | 2 | 364,019,712–364,134,400 | 364.0–364.1 | 347.2–347.3 | <120 MB |
| 100 MiB document, 30 s idle | 2 | 1,292,075,008–1,548,550,144 | 1,292.1–1,548.6 | 1,232.2–1,476.8 | <350 MB |
| Empty work folder, 30 s after scan | 2 | 77,185,024–77,217,792 | 77.2 | 73.6–73.7 | — |
| 1k-note folder, 30 s after scan | 2 | 83,935,232–84,279,296 | 83.9–84.3 | 80.0–80.4 | — |
| 10k-note folder, 30 s after scan | 2 | 61,931,520–88,670,208 | 61.9–88.7 | 59.1–84.6 | — |
| 10 visited notes, then 30 s idle | 1 | 131,219,456 | 131.2 | 125.2 | — |
| 100 visited notes, then 30 s idle | 1 | 141,885,440 | 141.9 | 135.3 | — |
| 1,000 visited notes, then 30 s idle | 1 | 255,098,880 | 255.1 | 243.2 | — |
| Open 100 MiB note, return to small note, then 30 s idle | 1 | 354,254,848 | 354.3 | 337.8 | — |

Work-folder RSS includes the first note that work-folder mode opens automatically; the 1k/10k
runs did not open the remaining indexed notes. Visit scenarios waited for each note's file load
to finish before recording the 10, 100 and 1,000 session checkpoints.

The 20-cycle long-running scenario switched between two work folders, opened two notes per
folder (including an image note), and performed 10 edit/undo/redo cycles per folder switch.
RSS was 101.2 MB after cycle 1, 108.3 MB after cycle 5, 108.7 MB after cycle 10, 108.7 MB
after cycle 15, 108.9 MB after cycle 20, and 108.1 MB after a final 30-second idle period.
This single run stayed near 108–109 MB after the first few cycles; it does not establish
behavior for arbitrarily long sessions.

## Budget and comparison assessment

- The ≤150 ms warm-start target was exceeded: median 164.306 ms and p95 175.748 ms in 30
  launches. The previous R0 reference is 165.934 ms, but its OS, Rust and GPUI versions differ,
  so the 10% relative-regression rule is not a valid conclusion across these conditions.
- The empty-editor, 10 MiB and 100 MiB RSS gates were exceeded. The 100 MiB document samples
  varied (about 1.29–1.55 GB decimal), and both exceed the 350 MB gate.
- A true cold-start result is unknown. `/usr/sbin/purge` returned nonzero, so those samples
  were labelled “OS cache not purged.” The 400 ms cold gate cannot be marked pass.
- The previous R0 reference also recorded 103.6 MB for a 10 MB document and 263.1 MB for a
  100 MB document. Those values were captured on macOS 26.5.1 with Rust 1.93.1 and GPUI 0.2.2;
  this run used macOS 26.6.2, Rust 1.98.1 and GPUI-pre 0.3.6. The absolute-gate failures are
  recorded without claiming a same-condition regression.
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
| Input source | `com.apple.keylayout.ABC` for startup; Japanese Kotoeri for the IME scenario |

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

Run the complete measurement set with a shared output directory:

```sh
HANE_MEASUREMENT_SAMPLES=30 \
HANE_MEASUREMENT_MEMORY_REPEATS=2 \
HANE_MEASUREMENT_IDLE_SECONDS=30 \
HANE_ISSUE23_LONGRUN_CYCLES=20 \
scripts/measure.sh all target/measure/issue23-current-main
```

The `all` command records startup scenarios, input scenarios, and RSS. It uses two separate
process trials for empty, 1/10/100 MiB documents and empty/1k/10k folders, and records the
10/100/1,000-note checkpoints, the large-to-small return scenario, and the 20-cycle long-run.
`HANE_MEASUREMENT_SAMPLES` controls startup sample count;
`HANE_MEASUREMENT_MEMORY_REPEATS` controls independent memory trials;
`HANE_MEASUREMENT_IDLE_SECONDS` controls stable-idle waits; and
`HANE_ISSUE23_LONGRUN_CYCLES` controls work-folder switch cycles.

The script attempts `/usr/sbin/purge` before each cold-labelled sample. A nonzero exit is
recorded as “OS cache not purged”; these are separate-process launches with warm OS/file
caches, not cold-cache measurements. Warm runs also use a new app process for every sample
without purging OS caches. Startup is measured from process start to the first `InputCapture`
paint. Work-folder scan completion time is captured when the background scanner returns, then
reported when the view publishes the results; the reported elapsed time excludes the later
polling delay and draft recovery. The automatically opened first note may still be loading then.

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
