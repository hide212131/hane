#!/usr/bin/env bash
# Merge per-commit GitHub status arrays into a single {sha: [...]}  map.
#
# The combined event log for a long-lived PR can exceed the ~128KiB jq
# --arg/--argjson argv limit (a single execve() argument is capped at
# MAX_ARG_STRLEN on Linux), which fails with "Argument list too long".
# Every payload here is read from a file (positional filename / --slurpfile),
# never passed as a literal argv string, so the merge has no such bound.
#
# Usage: merge_statuses_by_sha.sh <commits_file> <statuses_dir> <output_file>
#   commits_file  JSON array of commit objects (each with a "sha" field).
#   statuses_dir  Directory containing "<sha>.json" per commit, each a JSON
#                 array of status objects for that sha.
#   output_file   Destination for the merged {sha: [...]} JSON object.
set -euo pipefail

commits_file="$1"
statuses_dir="$2"
output_file="$3"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

echo '{}' > "$output_file"
while IFS= read -r sha; do
  sha_statuses_file="${statuses_dir}/${sha}.json"
  merged_file="${tmp_dir}/merged.json"
  jq --arg sha "$sha" --slurpfile statuses "$sha_statuses_file" \
    '. + {($sha): $statuses[0]}' "$output_file" > "$merged_file"
  mv "$merged_file" "$output_file"
done < <(jq -r '.[].sha' "$commits_file")
