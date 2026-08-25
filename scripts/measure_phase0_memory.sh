#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
results_dir=${1:-"$workspace_dir/target/phase0/ui"}
samples=${HANE_MEASUREMENT_SAMPLES:-30}
sample_indices=${HANE_MEMORY_SAMPLE_INDICES:-}
binary="$workspace_dir/target/release/hane"
fixtures="$workspace_dir/target/fixtures"
refresh_rate=${HANE_REFRESH_RATE_HZ:-"variable (CGDisplayMode reports 0)"}
input_source=$($script_dir/phase0_input.swift current-source)
ten_pid=""
hundred_pid=""

cleanup() {
    for pid in "$ten_pid" "$hundred_pid"; do
        if [ -n "$pid" ]; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$results_dir/memory_10mb" "$results_dir/memory_100mb"
rm -f "$results_dir/memory_10mb/metrics.csv" "$results_dir/memory_10mb/hane.log"
rm -f "$results_dir/memory_100mb/metrics.csv" "$results_dir/memory_100mb/hane.log"
cargo build --manifest-path "$workspace_dir/Cargo.toml" --release -p hane

launch_one() {
    scenario=$1
    csv_path=$2
    log_path=$3
    fixture=$4
    env \
        HANE_METRICS_SCENARIO="$scenario" \
        HANE_METRICS_CSV="$csv_path" \
        HANE_INPUT_SOURCE="$input_source" \
        HANE_REFRESH_RATE_HZ="$refresh_rate" \
        HANE_MEASURE_IDLE_RSS=1 \
        HANE_PHASE0_NO_FOCUS=1 \
        "$binary" "$fixture" 2>"$log_path" &
    launched_pid=$!
}

wait_ready() {
    pid=$1
    log=$2
    attempt=0
    while ! grep -q hane_ready "$log"; do
        kill -0 "$pid" 2>/dev/null || return 1
        attempt=$((attempt + 1))
        [ "$attempt" -lt 200 ] || return 1
        sleep 0.05
    done
}

if [ -n "$sample_indices" ]; then
    set -- $sample_indices
else
    set -- $(seq 1 "$samples")
fi
for iteration do
    ten_csv="$results_dir/memory_10mb/$iteration.csv"
    ten_log="$results_dir/memory_10mb/$iteration.log"
    hundred_csv="$results_dir/memory_100mb/$iteration.csv"
    hundred_log="$results_dir/memory_100mb/$iteration.log"
    launch_one "memory 10 MB" "$ten_csv" "$ten_log" "$fixtures/markdown_10mb.md"
    ten_pid=$launched_pid
    launch_one "memory 100 MB" "$hundred_csv" "$hundred_log" "$fixtures/markdown_100mb.md"
    hundred_pid=$launched_pid
    wait_ready "$ten_pid" "$ten_log"
    wait_ready "$hundred_pid" "$hundred_log"
    attempt=0
    while ! grep -q memory_idle_30s "$ten_csv" || ! grep -q memory_idle_30s "$hundred_csv"; do
        attempt=$((attempt + 1))
        [ "$attempt" -lt 70 ] || { echo "timed out waiting for idle RSS" >&2; exit 1; }
        sleep 1
    done
    cleanup
    ten_pid=""
    hundred_pid=""
    echo "memory sample $iteration"
done

python3 "$script_dir/aggregate_phase0_metrics.py" "$results_dir" "$results_dir/results.md"
