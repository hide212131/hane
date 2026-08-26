#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
results_dir=${1:-"$workspace_dir/target/phase0/ui"}
binary="$workspace_dir/target/release/hane"
fixtures="$workspace_dir/target/fixtures"
helper="$script_dir/phase0_input.swift"
warmup=${HANE_MEASUREMENT_WARMUP:-5}
samples=${HANE_MEASUREMENT_SAMPLES:-30}
refresh_rate=${HANE_REFRESH_RATE_HZ:-"variable (CGDisplayMode reports 0)"}
original_input_source=$($helper current-source)
ascii_source=${HANE_ASCII_INPUT_SOURCE:-com.apple.keylayout.ABC}
japanese_source=${HANE_JAPANESE_INPUT_SOURCE:-com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese}

mkdir -p "$results_dir"
cargo run --manifest-path "$workspace_dir/Cargo.toml" --release -p hane-benchmark --bin hane-bench -- fixtures >/dev/null
cargo build --manifest-path "$workspace_dir/Cargo.toml" --release -p hane --features instrument

app_pid=""
cleanup() {
    if [ -n "$app_pid" ]; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    "$helper" select-source "$original_input_source" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

wait_ready() {
    log=$1
    attempt=0
    while ! grep -q hane_ready "$log"; do
        if ! kill -0 "$app_pid" 2>/dev/null; then
            cat "$log" >&2
            exit 1
        fi
        attempt=$((attempt + 1))
        if [ "$attempt" -ge 200 ]; then
            echo "timed out waiting for first InputCapture paint" >&2
            exit 1
        fi
        sleep 0.05
    done
}

launch() {
    scenario=$1
    csv_path=$2
    log_path=$3
    fixture=${4:-}
    cursor_offset=${5:-}
    background=${6:-}
    idle=${7:-}
    gate=${8:-}
    input_source=${9:-$ascii_source}
    autoscroll=${10:-}
    if [ -n "$fixture" ]; then
        set -- "$binary" "$fixture"
        measurement_empty=""
    else
        set -- "$binary"
        measurement_empty=1
    fi
    env \
        HANE_METRICS_SCENARIO="$scenario" \
        HANE_METRICS_CSV="$csv_path" \
        HANE_METRICS_GATE="$gate" \
        HANE_INPUT_SOURCE="$input_source" \
        HANE_REFRESH_RATE_HZ="$refresh_rate" \
        HANE_MEASUREMENT_CURSOR_OFFSET="$cursor_offset" \
        HANE_BACKGROUND_PRESENTATION="$background" \
        HANE_MEASURE_IDLE_RSS="$idle" \
        HANE_AUTOSCROLL="$autoscroll" \
        HANE_MEASUREMENT_EMPTY="$measurement_empty" \
        "$@" 2>"$log_path" &
    app_pid=$!
    wait_ready "$log_path"
}

stop_app() {
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
    app_pid=""
}

startup_series() {
    scenario=$1
    directory=$2
    purge_cache=$3
    mkdir -p "$directory"
    iteration=1
    while [ "$iteration" -le "$samples" ]; do
        if [ "$purge_cache" = true ]; then
            /usr/sbin/purge >/dev/null 2>&1 || true
        fi
        launch "$scenario" "$directory/$iteration.csv" "$directory/$iteration.log"
        stop_app
        iteration=$((iteration + 1))
    done
}

input_scenario() {
    scenario=$1
    name=$2
    mode=$3
    fixture=$4
    offset=${5:-0}
    background=${6:-}
    if [ "$mode" = scroll ] || [ "$mode" = scroll-input ]; then
        autoscroll=1
    else
        autoscroll=""
    fi
    directory="$results_dir/$name"
    gate="$directory/measure"
    mkdir -p "$directory"
    rm -f "$gate"
    if [ "$mode" = ime ]; then
        "$helper" select-source "$japanese_source"
        input_source=$japanese_source
    else
        "$helper" select-source "$ascii_source"
        input_source=$ascii_source
    fi
    launch "$scenario" "$directory/metrics.csv" "$directory/hane.log" "$fixture" "$offset" "$background" "" "$gate" "$input_source" "$autoscroll"
    "$helper" "$mode" "$app_pid" "$warmup"
    : > "$gate"
    "$helper" "$mode" "$app_pid" "$samples"
    sleep 0.5
    stop_app
    if [ "$mode" = ime ]; then
        "$helper" select-source "$ascii_source"
    fi
}

memory_scenario() {
    scenario=$1
    name=$2
    fixture=$3
    directory="$results_dir/$name"
    mkdir -p "$directory"
    launch "$scenario" "$directory/metrics.csv" "$directory/hane.log" "$fixture" "0" "" "1"
    attempt=0
    while ! grep -q memory_idle_30s "$directory/metrics.csv"; do
        attempt=$((attempt + 1))
        if [ "$attempt" -ge 70 ]; then
            echo "timed out waiting for 30 second idle RSS" >&2
            exit 1
        fi
        sleep 1
    done
    stop_app
}

hundred_size=$(wc -c < "$fixtures/markdown_100mb.md" | tr -d ' ')
middle_offset=$((hundred_size / 2))
while ! python3 -c 'import sys; stream=open(sys.argv[1],"rb"); stream.seek(int(sys.argv[2])); data=stream.read(1); sys.exit(0 if not data or data[0] & 0xC0 != 0x80 else 1)' "$fixtures/markdown_100mb.md" "$middle_offset" 2>/dev/null; do
    middle_offset=$((middle_offset - 1))
done

paragraphs_size=$(wc -c < "$fixtures/paragraphs_100k.md" | tr -d ' ')
paragraphs_middle_offset=$((paragraphs_size / 2))
while ! python3 -c 'import sys; stream=open(sys.argv[1],"rb"); stream.seek(int(sys.argv[2])); data=stream.read(1); sys.exit(0 if not data or data[0] & 0xC0 != 0x80 else 1)' "$fixtures/paragraphs_100k.md" "$paragraphs_middle_offset" 2>/dev/null; do
    paragraphs_middle_offset=$((paragraphs_middle_offset - 1))
done

startup_series "empty warm startup" "$results_dir/startup_warm" false
if /usr/sbin/purge >/dev/null 2>&1; then
    startup_series "empty cold startup" "$results_dir/startup_cold" true
else
    startup_series "empty cold startup (OS cache not purged)" "$results_dir/startup_cold_unpurged" false
fi
input_scenario "normal ASCII input" normal_ascii ascii "$fixtures/japanese.md" 0
input_scenario "real Japanese IME composition to commit" ime ime "$fixtures/japanese.md" 0
input_scenario "100 MB input at start" hundred_start ascii "$fixtures/markdown_100mb.md" 0
input_scenario "100 MB input at middle" hundred_middle ascii "$fixtures/markdown_100mb.md" "$middle_offset"
input_scenario "100 MB input at end" hundred_end ascii "$fixtures/markdown_100mb.md" "$hundred_size"
input_scenario "100 MB scroll only" scroll scroll "$fixtures/markdown_100mb.md" 0
input_scenario "100 MB input while scrolling" scroll_input scroll-input "$fixtures/markdown_100mb.md" 0
input_scenario "100k paragraphs input at start" paragraphs_start ascii "$fixtures/paragraphs_100k.md" 0
input_scenario "100k paragraphs input at middle" paragraphs_middle ascii "$fixtures/paragraphs_100k.md" "$paragraphs_middle_offset"
input_scenario "100k paragraphs input at end" paragraphs_end ascii "$fixtures/paragraphs_100k.md" "$paragraphs_size"
input_scenario "100k paragraphs scroll only" paragraphs_scroll scroll "$fixtures/paragraphs_100k.md" 0
input_scenario "100k paragraphs input while scrolling" paragraphs_scroll_input scroll-input "$fixtures/paragraphs_100k.md" 0
input_scenario "input during background presentation update" background_input ascii "$fixtures/japanese.md" 0 1
memory_scenario "memory 10 MB" memory_10mb "$fixtures/markdown_10mb.md"
memory_scenario "memory 100 MB" memory_100mb "$fixtures/markdown_100mb.md"

python3 "$script_dir/aggregate_phase0_metrics.py" "$results_dir" "$results_dir/results.md"
