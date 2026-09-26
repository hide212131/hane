# Hane current-main startup and RSS baseline

Issue: [#23](https://github.com/hide212131/hane/issues/23)

Current `main` at report preparation: `b4061b9d43cd0c14dc4894cc4e27fa9227bd8199`

Measured product source: `174558e522feb7ed17e9cc7567dcd71e0499a7d6`. The intervening `main` changes are documentation and agent instructions only; product code is unchanged.

Measurement harness revision: `5576d8e9692fb629fc242170ccaab73eb1314b5c`.

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
| Empty warm startup | 30 | 188.597 ms | 244.299 ms | New process per launch |
| Empty unpurged startup | 30 | 189.042 ms | 211.348 ms | OS cache was not purged; not a true cold result |
| 1 KiB Markdown startup | 30 | 200.667 ms | 222.585 ms | File-open median 71.150 ms |
| 100 MiB Markdown startup | 30 | 385.340 ms | 418.998 ms | File-open median 239.404 ms |
| Empty work-folder startup | 30 | 192.423 ms | 224.635 ms | Scan completion median 313.440 ms, p95 361.887 ms |
| 1k-note work-folder startup | 30 | 192.408 ms | 225.242 ms | Scan completion median 322.066 ms, p95 362.686 ms |
| 10k-note work-folder startup | 30 | 186.819 ms | 203.377 ms | Scan completion median 659.870 ms, p95 692.250 ms |

For 10k notes, the first editor paint was ready at a median of 186.819 ms while the scan
completed at a median of 659.870 ms (p95 692.250 ms). Folder discovery completed after the
editor became ready in these runs.

| RSS scenario | Samples | Measured bytes | MB (decimal) | MiB (binary) | Initial budget |
|---|---:|---:|---:|---:|---:|
| Empty editor, 30 s idle | 2 | 96,993,280–97,796,096 | 97.0–97.8 | 92.5–93.3 | <65 MiB |
| 1 MiB document, 30 s idle | 2 | 121,503,744–122,175,488 | 121.5–122.2 | 115.9–116.5 | — |
| 10 MiB document, 30 s idle | 2 | 296,878,080–370,130,944 | 296.9–370.1 | 283.1–353.0 | <120 MB |
| 100 MiB document, 30 s idle | 2 | 1,449,951,232–2,155,495,424 | 1,450.0–2,155.5 | 1,382.8–2,055.6 | <350 MB |
| Empty work folder, 30 s after scan | 2 | 87,212,032–87,425,024 | 87.2–87.4 | 83.2–83.4 | — |
| 1k-note folder, 30 s after scan | 2 | 123,076,608–123,568,128 | 123.1–123.6 | 117.4–117.8 | — |
| 10k-note folder, 30 s after scan | 2 | 418,693,120–477,822,976 | 418.7–477.8 | 399.3–455.7 | — |
| 10 visited notes, then 30 s idle | 1 | 132,546,560 | 132.5 | 126.4 | — |
| 100 visited notes, then 30 s idle | 1 | 144,015,360 | 144.0 | 137.3 | — |
| 1,000 visited notes, then 30 s idle | 1 | 188,628,992 | 188.6 | 179.9 | — |
| Open 100 MiB note, return to small note, then 30 s idle | 1 | 354,680,832 | 354.7 | 338.2 | — |

Work-folder RSS includes the first note that work-folder mode opens automatically; the 1k/10k
runs did not open the remaining indexed notes. Visit scenarios waited for each note's file load
to finish before recording the 10, 100 and 1,000 session checkpoints.

The 20-cycle long-running scenario switched between two work folders, opened two notes per
folder (including an image note), and performed 10 edit/undo/redo cycles per folder switch.
RSS was 97.6 MB after cycle 1, 108.1 MB after cycle 5, 108.3 MB after cycle 10, 108.4 MB
after cycle 15, 108.8 MB after cycle 20, and 108.2 MB after a final 30-second idle period.
This single run stayed near 108–109 MB after the first few cycles; it does not establish
behavior for arbitrarily long sessions.

## Budget and comparison assessment

- The ≤150 ms warm-start target was exceeded: median 188.597 ms and p95 244.299 ms in 30
  launches. The previous R0 reference is 165.934 ms, but its OS, Rust and GPUI versions differ,
  so the 10% relative-regression rule is not a valid conclusion across these conditions.
- The empty-editor, 10 MiB and 100 MiB RSS gates were exceeded. The 100 MiB document samples
  varied substantially (about 1.45–2.16 GB decimal), but both exceed the 350 MB gate.
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
