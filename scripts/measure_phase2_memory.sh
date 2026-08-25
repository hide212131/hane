#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
results_dir=${1:-"$workspace_dir/target/phase2/ui"}

"$script_dir/measure_phase0_memory.sh" "$results_dir"
python3 "$script_dir/aggregate_phase0_metrics.py" --phase 2 "$results_dir" "$results_dir/results.md"
