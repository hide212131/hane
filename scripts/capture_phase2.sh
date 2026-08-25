#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
output=${1:-"$workspace_dir/target/captures/phase2-markdown-presentation.png"}
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/hane-phase2-capture.XXXXXX")
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

cargo build --manifest-path "$workspace_dir/Cargo.toml" -p hane
"$workspace_dir/target/debug/hane" 2> "$log" &
app_pid=$!

attempt=0
while ! grep -q 'hane_ready' "$log"; do
    kill -0 "$app_pid" 2>/dev/null || { cat "$log" >&2; exit 1; }
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
