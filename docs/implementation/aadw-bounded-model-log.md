# AADW Claude execution log bounds follow-up

This dedicated operations change follows the safe measurement work in PR #498. It does not change Hane product code or the requirement to verify positive Sonnet 5.5 token usage.

## Evidence

AADW run [39024106716](https://github.com/hide212131/hane/actions/runs/39024106716) used the requested `claude-sonnet-5-5` session model and completed the worker step. The trusted verifier reported a regular file, not a symlink, with size 52,504,659 bytes. The verifier stopped at its 16 MiB bound before parsing the execution file, so positive per-model token usage was not verified. The action's result summary showed a `modelUsage` key, but its abbreviated log does not establish positive usage.

## Required behavior

Keep a finite maximum file size and the current regular-file / non-symlink protections. Do not print or upload execution-log contents. Update trusted verification so an execution file within the chosen finite bound can be checked for the primary session model and positive Sonnet 5.5 token usage without unbounded memory use. Keep failing closed when the file is missing, non-regular, a symlink, malformed, oversized, or lacks positive expected-model usage. Record only bounded metadata and boolean/count diagnostics.

Add focused tests in the existing `.github/tests/test_claude_model_verification.py` for valid and rejected file cases, including a valid execution log larger than the former 16 MiB limit. The current `.github/workflows/ci.yml` already runs this test, so no workflow change is needed. Keep the change to the trusted verifier, its existing focused test, and this operational note; do not modify product Rust code or workflow permissions.

## Verification path

The verifier script and its tests are protected under `.github/`. The standard Claude worker finalizer rejects those paths, so the authorized operator applies this operations-only change directly on the dedicated PR branch. Do not relax the product worker's protected-path guard. Current PR CI runs the existing Python test through the trusted CI workflow.
