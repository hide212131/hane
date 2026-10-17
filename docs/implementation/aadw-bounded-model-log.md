# AADW Claude execution log bounds follow-up

This dedicated operations change follows the safe measurement work in PR #498. It does not change Hane product code or the requirement to verify positive Sonnet 5.5 token usage.

## Evidence

AADW run [39024106716](https://github.com/hide212131/hane/actions/runs/39024106716) used the requested `claude-sonnet-5-5` session model and completed the worker step. The trusted verifier reported a regular file, not a symlink, with size 52,504,659 bytes. The verifier stopped at its 16 MiB bound before parsing the execution file, so positive per-model token usage was not verified. The action's result summary showed a `modelUsage` key, but its abbreviated log does not establish positive usage.

## Required behavior

Keep a finite maximum file size and the current regular-file / non-symlink protections. Do not print or upload execution-log contents. Update trusted verification so an execution file within the chosen finite bound can be checked for the primary session model and positive Sonnet 5.5 token usage without unbounded memory use. Keep failing closed when the file is missing, non-regular, a symlink, malformed, oversized, or lacks positive expected-model usage. Record only bounded metadata and boolean/count diagnostics.

Add focused tests for valid and rejected file cases, including a valid execution log larger than the former 16 MiB limit. The change must stay under `.github/scripts/` and its focused tests plus this operational note; it must not modify product Rust code or workflow permissions.
