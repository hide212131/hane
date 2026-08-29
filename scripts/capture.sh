#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
scenario=${1:?"usage: scripts/capture.sh <scenario> [output]"}
output=${2:-"$workspace_dir/target/captures/$scenario.png"}
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/hane-capture.XXXXXX")
log="$temporary_dir/hane.log"
app_pid=""
fixture=""
instrument=""

cleanup() {
    if [ -n "$app_pid" ]; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    rm -r "$temporary_dir"
}
trap cleanup EXIT HUP INT TERM

case "$scenario" in
    editor)
        fixture=${HANE_CAPTURE_FIXTURE:-}
        ;;
    cursor-boundary)
        fixture="$temporary_dir/cursor-boundary.md"
        printf 'first line\nsecond line\n' > "$fixture"
        instrument=1
        export HANE_MEASUREMENT_CURSOR_OFFSET=${HANE_CAPTURE_CURSOR_OFFSET:-11}
        ;;
    cursor-scroll)
        fixture="$temporary_dir/forty-lines.md"
        line=1
        while [ "$line" -le 40 ]; do
            printf 'line %02d — scroll verification\n' "$line"
            line=$((line + 1))
        done > "$fixture"
        instrument=1
        export HANE_DEV_CURSOR_DOWN=${HANE_CAPTURE_CURSOR_DOWN:-32}
        ;;
    *)
        echo "unknown scenario: $scenario" >&2
        echo "available scenarios: editor, cursor-boundary, cursor-scroll" >&2
        exit 2
        ;;
esac

if [ -n "$instrument" ]; then
    cargo build --manifest-path "$workspace_dir/Cargo.toml" -p hane --features instrument
else
    cargo build --manifest-path "$workspace_dir/Cargo.toml" -p hane
fi
cd "$workspace_dir"
if [ -n "$fixture" ]; then
    "$workspace_dir/target/debug/hane" "$fixture" 2> "$log" &
else
    "$workspace_dir/target/debug/hane" 2> "$log" &
fi
app_pid=$!

attempt=0
while ! grep -q 'hane_ready' "$log"; do
    kill -0 "$app_pid" 2>/dev/null || { sed -n '1,160p' "$log" >&2; exit 1; }
    attempt=$((attempt + 1))
    [ "$attempt" -lt 100 ] || { echo "timed out waiting for Hane" >&2; exit 1; }
    sleep 0.1
done

window_id=""
attempt=0
while [ -z "$window_id" ]; do
    window_id=$(swift "$script_dir/window_id.swift" "$app_pid" 2>/dev/null || true)
    attempt=$((attempt + 1))
    [ "$attempt" -lt 20 ] || { echo "could not find the Hane window" >&2; exit 1; }
    sleep 0.1
done

mkdir -p "$(dirname -- "$output")"
screencapture -x -l "$window_id" "$output"
output_dir=$(CDPATH= cd -- "$(dirname -- "$output")" && pwd)
echo "$output_dir/$(basename -- "$output")"
