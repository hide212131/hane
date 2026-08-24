#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
output=${1:-"$workspace_dir/target/captures/cursor-line-boundary.png"}
cursor_offset=${HANE_CAPTURE_CURSOR_OFFSET:-11}
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/hane-capture.XXXXXX")
fixture="$temporary_dir/cursor-boundary.md"
log="$temporary_dir/hane.log"
app_pid=""

cleanup() {
    if [ -n "$app_pid" ]; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    rm -r "$temporary_dir"
}
trap cleanup EXIT HUP INT TERM

printf 'first line\nsecond line\n' > "$fixture"

cargo build --manifest-path "$workspace_dir/Cargo.toml" -p hane
HANE_DEV_CURSOR_OFFSET="$cursor_offset" "$workspace_dir/target/debug/hane" "$fixture" 2> "$log" &
app_pid=$!

attempt=0
while ! grep -q 'hane_ready' "$log"; do
    if ! kill -0 "$app_pid" 2>/dev/null; then
        cat "$log" >&2
        exit 1
    fi
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 100 ]; then
        echo "timed out waiting for Hane" >&2
        cat "$log" >&2
        exit 1
    fi
    sleep 0.1
done

window_id=""
attempt=0
while [ -z "$window_id" ]; do
    window_id=$(swift "$script_dir/window_id.swift" "$app_pid" 2>/dev/null || true)
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 20 ]; then
        echo "could not find the Hane window for PID $app_pid" >&2
        exit 1
    fi
    sleep 0.1
done

mkdir -p "$(dirname -- "$output")"
screencapture -x -l "$window_id" "$output"

output_dir=$(CDPATH= cd -- "$(dirname -- "$output")" && pwd)
echo "$output_dir/$(basename -- "$output")"
