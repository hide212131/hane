#!/bin/sh
set -eu

export PATH="${HOME}/.cargo/bin:${PATH}"
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$workspace_dir"
scenario=${1:?"usage: scripts/measure.sh <scenario> [results-dir]"}
results_dir=${2:-"$workspace_dir/target/measure/$scenario"}
binary="$workspace_dir/target/release/hane"
fixtures="$workspace_dir/target/fixtures"
issue23_fixtures="$fixtures/issue23"
helper="$script_dir/phase0_input.swift"
warmup=${HANE_MEASUREMENT_WARMUP:-5}
samples=${HANE_MEASUREMENT_SAMPLES:-30}
memory_repeats=${HANE_MEASUREMENT_MEMORY_REPEATS:-2}
issue23_visit_counts=${HANE_ISSUE23_VISIT_COUNTS:-10,100,1000}
issue23_longrun_cycles=${HANE_ISSUE23_LONGRUN_CYCLES:-20}
measurement_feature=${HANE_MEASUREMENT_FEATURE:-instrument}
refresh_rate=${HANE_REFRESH_RATE_HZ:-"variable (CGDisplayMode reports 0)"}
original_input_source=$($helper current-source)
ascii_source=${HANE_ASCII_INPUT_SOURCE:-com.apple.keylayout.ABC}
japanese_source=${HANE_JAPANESE_INPUT_SOURCE:-com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese}

mkdir -p "$results_dir"
cargo +1.98.1 run --locked --manifest-path "$workspace_dir/Cargo.toml" --release -p hane-benchmark --bin hane-bench -- fixtures >/dev/null
python3 "$script_dir/prepare_issue23_fixtures.py" >/dev/null
python3 "$script_dir/prepare_issue23_fixtures.py" --verify >/dev/null
case "$measurement_feature" in
    instrument|timing-probe) ;;
    *)
        echo "HANE_MEASUREMENT_FEATURE must be instrument or timing-probe" >&2
        exit 2
        ;;
esac
cargo +1.98.1 build --locked --manifest-path "$workspace_dir/Cargo.toml" --release -p hane --features "$measurement_feature"

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

wait_marker() {
    log=$1
    marker=$2
    max_attempts=${3:-1200}
    attempt=0
    while ! grep -q "$marker" "$log"; do
        if ! kill -0 "$app_pid" 2>/dev/null; then
            cat "$log" >&2
            exit 1
        fi
        attempt=$((attempt + 1))
        if [ "$attempt" -ge "$max_attempts" ]; then
            echo "timed out waiting for $marker" >&2
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
    fixture=${4:-}
    measure_folder=${5:-false}
    mkdir -p "$directory"
    rm -f "$directory"/*.csv "$directory"/*.log
    iteration=1
    while [ "$iteration" -le "$samples" ]; do
        if [ "$purge_cache" = true ]; then
            /usr/sbin/purge >/dev/null 2>&1 || true
        fi
        if [ "$measure_folder" = true ]; then
            HANE_MEASURE_WORK_FOLDER=1 launch "$scenario" "$directory/$iteration.csv" "$directory/$iteration.log" "$fixture"
            wait_marker "$directory/$iteration.log" hane_work_folder_ready
        else
            launch "$scenario" "$directory/$iteration.csv" "$directory/$iteration.log" "$fixture"
        fi
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
    rm -rf "$directory"/trial_*
    iteration=1
    while [ "$iteration" -le "$memory_repeats" ]; do
        trial_dir="$directory/trial_$iteration"
        mkdir -p "$trial_dir"
        launch "$scenario" "$trial_dir/metrics.csv" "$trial_dir/hane.log" "$fixture" "0" "" "1"
        wait_marker "$trial_dir/metrics.csv" memory_idle_30s
        stop_app
        iteration=$((iteration + 1))
    done
}

folder_memory_scenario() {
    scenario=$1
    name=$2
    folder=$3
    directory="$results_dir/$name"
    mkdir -p "$directory"
    rm -rf "$directory"/trial_*
    iteration=1
    while [ "$iteration" -le "$memory_repeats" ]; do
        trial_dir="$directory/trial_$iteration"
        mkdir -p "$trial_dir"
        HANE_MEASURE_WORK_FOLDER=1 HANE_MEASUREMENT_IDLE_SECONDS="${HANE_MEASUREMENT_IDLE_SECONDS:-30}" \
            launch "$scenario" "$trial_dir/metrics.csv" "$trial_dir/hane.log" "$folder"
        wait_marker "$trial_dir/hane.log" memory_work_folder_idle
        stop_app
        iteration=$((iteration + 1))
    done
}

visit_memory_scenario() {
    scenario=$1
    name=$2
    folder=$3
    counts=$4
    directory="$results_dir/$name"
    mkdir -p "$directory"
    rm -rf "$directory"/trial_*
    trial_dir="$directory/trial_1"
    mkdir -p "$trial_dir"
    HANE_MEASUREMENT_VISIT_COUNTS="$counts" HANE_MEASUREMENT_IDLE_SECONDS="${HANE_MEASUREMENT_IDLE_SECONDS:-30}" \
        launch "$scenario" "$trial_dir/metrics.csv" "$trial_dir/hane.log" "$folder"
    last_count=$(printf '%s\n' "$counts" | tr ',' '\n' | sort -n | tail -n 1)
    wait_attempts=1200
    if [ "$last_count" -ge 1000 ]; then
        wait_attempts=12000
    fi
    wait_marker "$trial_dir/hane.log" "label=memory_after_${last_count}_notes" "$wait_attempts"
    stop_app
}

longrun_scenario() {
    scenario=$1
    name=$2
    folder_a=$3
    folder_b=$4
    cycles=$5
    directory="$results_dir/$name"
    mkdir -p "$directory"
    rm -f "$directory/metrics.csv" "$directory/hane.log"
    HANE_MEASUREMENT_CYCLE_FOLDERS="$folder_a;$folder_b" \
        HANE_MEASUREMENT_CYCLES="$cycles" HANE_MEASUREMENT_IDLE_SECONDS="${HANE_MEASUREMENT_IDLE_SECONDS:-30}" \
        launch "$scenario" "$directory/metrics.csv" "$directory/hane.log" "$folder_a"
    wait_marker "$directory/hane.log" hane_longrun_complete
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

run_startup() {
    startup_series "empty warm startup" "$results_dir/startup_warm" false ""
    if /usr/sbin/purge >/dev/null 2>&1; then
        startup_series "empty cold startup" "$results_dir/startup_cold" true ""
    else
        startup_series "empty cold startup (OS cache not purged)" "$results_dir/startup_cold_unpurged" false ""
    fi
    startup_series "small document warm startup" "$results_dir/startup_small" false "$issue23_fixtures/small.md"
    startup_series "100 MiB document warm startup" "$results_dir/startup_100mb" false "$fixtures/markdown_100mb.md"
    startup_series "empty work folder warm startup" "$results_dir/startup_folder_empty" false "$issue23_fixtures/folder_empty" true
    startup_series "1k work folder warm startup" "$results_dir/startup_folder_1k" false "$issue23_fixtures/folder_1k" true
    startup_series "10k work folder warm startup" "$results_dir/startup_folder_10k" false "$issue23_fixtures/folder_10k" true
}

run_input() {
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
}

run_memory() {
    memory_scenario "memory empty editor" memory_empty ""
    memory_scenario "memory 1 MiB" memory_1mb "$fixtures/markdown_1mb.md"
    memory_scenario "memory 10 MiB" memory_10mb "$fixtures/markdown_10mb.md"
    memory_scenario "memory 100 MiB" memory_100mb "$fixtures/markdown_100mb.md"
    folder_memory_scenario "memory empty work folder" memory_folder_empty "$issue23_fixtures/folder_empty"
    folder_memory_scenario "memory 1k work folder" memory_folder_1k "$issue23_fixtures/folder_1k"
    folder_memory_scenario "memory 10k work folder" memory_folder_10k "$issue23_fixtures/folder_10k"
    visit_memory_scenario "memory after visiting notes" memory_visits_1k "$issue23_fixtures/folder_1k" "$issue23_visit_counts"
    visit_memory_scenario "memory after large then small" memory_large_then_small "$issue23_fixtures/large_then_small" 2
    longrun_scenario "memory long-running switches, edits, and images" memory_longrun "$issue23_fixtures/switch_a" "$issue23_fixtures/switch_b" "$issue23_longrun_cycles"
}

run_workfolder() {
    folder_memory_scenario "memory empty work folder" memory_folder_empty "$issue23_fixtures/folder_empty"
    folder_memory_scenario "memory 1k work folder" memory_folder_1k "$issue23_fixtures/folder_1k"
    folder_memory_scenario "memory 10k work folder" memory_folder_10k "$issue23_fixtures/folder_10k"
    visit_memory_scenario "memory after visiting notes" memory_visits_1k "$issue23_fixtures/folder_1k" "$issue23_visit_counts"
    visit_memory_scenario "memory after large then small" memory_large_then_small "$issue23_fixtures/large_then_small" 2
    longrun_scenario "memory long-running switches, edits, and images" memory_longrun "$issue23_fixtures/switch_a" "$issue23_fixtures/switch_b" "$issue23_longrun_cycles"
}

run_visits_and_longrun() {
    visit_memory_scenario "memory after visiting notes" memory_visits_1k "$issue23_fixtures/folder_1k" "$issue23_visit_counts"
    visit_memory_scenario "memory after large then small" memory_large_then_small "$issue23_fixtures/large_then_small" 2
    longrun_scenario "memory long-running switches, edits, and images" memory_longrun "$issue23_fixtures/switch_a" "$issue23_fixtures/switch_b" "$issue23_longrun_cycles"
}

run_comparison() {
    # These scenarios need no app-side development operation, so the probe and
    # instrument binaries receive identical externally injected input.
    input_scenario "normal ASCII input" normal_ascii ascii "$fixtures/japanese.md" 0
    input_scenario "100 MB input at start" hundred_start ascii "$fixtures/markdown_100mb.md" 0
}

case "$scenario" in
    all) run_startup; run_input; run_memory ;;
    startup) run_startup ;;
    input) run_input ;;
    memory) run_memory ;;
    workfolder) run_workfolder ;;
    visits) run_visits_and_longrun ;;
    comparison) run_comparison ;;
    *)
        echo "unknown scenario: $scenario" >&2
        echo "available scenarios: all, startup, input, memory, workfolder, visits, comparison" >&2
        exit 2
        ;;
esac

python3 "$script_dir/aggregate_metrics.py" "$results_dir" "$results_dir/results.md"
