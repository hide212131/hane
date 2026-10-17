# Issue #305 implementation handoff

Updated 2026-10-17 from current GitHub state. This commit refreshes handoff evidence only; this document is the only file changed by the refresh. Values below are snapshots at that time and must be distinguished from the live head and status, which the Commander re-reads before each request.

## Current state

- Issue #305 remains open. All five acceptance criteria are unmet.
- Design PR [#496](https://github.com/hide212131/hane/pull/496) is merged. Its final design head was 14def973e496c39b503c006db21fd187718ad683. CodeRabbit full review completed with no actionable findings. CI run [39002710437](https://github.com/hide212131/hane/actions/runs/39002710437) succeeded; macOS and Windows Rust jobs were skipped because the change was documentation-only.
- Current main (confirmed 2026-10-17) is 777689ae02d78c90aabbd1af1a9619aea6f89035, five commits ahead of the design reading baseline 2164b895e849eef4de8c80c3696c0fd8e00c496b. The difference is `.github/scripts/verify_claude_model.py` and documentation only (including #509, which is docs-only); there is no product Rust source or test change, so the product source used by the design is unchanged.
- Implementation PR [#497](https://github.com/hide212131/hane/pull/497) is open/Draft. The head targeted by this refresh is cf4f5e7238622915efd8dcd8085b0febb5decb11 (a point-in-time observation, not a guarantee about the live head). Compared with current main it is ahead 10 / behind 1 with merge base e82b9a9fe42280bfa78497746bcd7e21c73430c5; the one behind commit is the docs-only #509. Its diff is two files: the C01-C08 baseline tests in `crates/ui/src/view/input_routing.rs` and this handoff document. No production implementation has started. The live head and checks must be re-read before each request.
- Document-refresh run [39022873785](https://github.com/hide212131/hane/actions/runs/39022873785) succeeded for Sonnet 5.5 confirmation, the trusted finalizer, current-head workspace regression tests, and clippy. Earlier exact-head CI (for example run [39020139079](https://github.com/hide212131/hane/actions/runs/39020139079) on the older head 1e7843a) is a past snapshot and is not evidence for the current head. Main's #509 is documentation-only and does not overlap source, tests, or workflows.
- Review record: [Codex review](https://github.com/hide212131/hane/pull/497#pullrequestreview-5557278651) is a user-designated substitute review. It found no code issues in C01-C08; the stale state in this handoff was its only finding. CodeRabbit hit its rate limit twice and did not perform a full review. Do not confuse the user-designated substitute review with a CodeRabbit review.
- The earlier production implementation request for PR #497 (run [39020761594](https://github.com/hide212131/hane/actions/runs/39020761594)) was cancelled when a concurrent writer was detected. Patch, finalizer, and verification made no product change. Run [39017012794](https://github.com/hide212131/hane/actions/runs/39017012794) was a diagnostic of the current Claude measurement path that confirmed Sonnet 5.5; it does not retroactively prove the model of older runs.

## Parallel PR #495

PR [#495](https://github.com/hide212131/hane/pull/495) is open/ready and unmerged. At the time of this refresh its head is `76d53cbf5b33a51ed91dfa259ced76403064872a`. Compared with current main it is ahead 18 / behind 5 with merge base 2164b895e849eef4de8c80c3696c0fd8e00c496b and changes 13 files. Its diff overlaps this work in `crates/ui/src/actions.rs`, `crates/ui/src/view.rs`, and `crates/ui/src/view/input_routing.rs`. Earlier handoff versions recorded older heads (`c38b9d2...`, `89e2f9d...`); those are superseded.

The normal CI run [39022930215](https://github.com/hide212131/hane/actions/runs/39022930215) for the current head succeeded for the macOS and Windows workspace tests and clippy. Two review threads are resolved, and the older README thread is resolved/outdated. The latest CodeRabbit review submission was made at an earlier head, so do not assume a current-head CodeRabbit review has completed.

#495 is not merged, so its code is not accepted. Do not import it, do not depend on it, and do not add C09. Until it is accepted, base any implementation on current main only. Re-check its head and merge state before starting implementation. If it is accepted before the #305 cutover is complete, re-read its accepted head and diff, then reassess the overlap before continuing.

## Next implementation entry

After this document update, the Commander re-reads the PR head, current main, Issue #305, #495, CI/review, and active AADW runs, then decides whether to start the implementation request. The next implementation is a separate AADW request against the new current head of the same PR #497. The adopted spec `docs/refactor-rf3b-short-text-input-implementation-spec.md` is authoritative. Keep C01-C08 passing and proceed with the ShortTextInput model (M01-M15) and the complete filter/rename cutover. If #495 remains unaccepted, use current-main code only and do not carry in its changes; if it has been accepted, re-check it and stop.

The tests-only step (C01-C08 baseline) is already on the PR branch. The implementation request must not add a synchronized old/new state period. Keep input routing, focus, pending rename, file operations, clipboard integration, rendering bounds, notifications, and list updates with their existing owners. The implementation must remove the duplicate state and helpers.

The C01-C08 baseline checks, for reference:

- C01: same-string replacement and empty deletion preserve the filter scroll reset and rename notification side effects; body content, revision, and history stay unchanged.
- C02: pending-rename Copy, Cut, Shift-arrows, SelectAll, Home/End, document-edge navigation/selection, Backspace/Delete, and Paste retain the entry-specific behavior in spec section 6.2.
- C03: both callbacks receiving CRLF are not treated as an empty OS cancel; verify the ordinary result after CR/LF removal.
- C04: a reversed selection followed by multiple mark updates and Windows-style or macOS-style cancel restores the original text, range, and direction without changing focus or the other input target.
- C05: filter and rename re-click behavior during composition, plus filter blur, remains unchanged.
- C06: deletion before a combining sequence or emoji is correct through the rename action route, with body text and file extension unchanged.
- C07: non-focused filter rendering hides only the painted selection, while the model selection remains; forward and reversed candidate-position queries use the moving end.
- C08: the late-body-cancel guard covers replace_text_in_range(Some(marked_range), "") and continues to block later non-empty callbacks.

C09 (document-start/end actions on short fields) applies only if #495 is accepted first; do not add it based on the unaccepted branch.

## Completion evidence and Issue state

The spec's M01-M15 unit tests, C01-C09 action/callback checks, and G01-G06 native GUI checks are plans, not completed evidence, except that the C01-C08 baseline tests are present and passed in the exact-head CI above. The final implementation must meet the Issue acceptance criteria with current-head CI, review, and focused macOS/Windows GUI evidence. This handoff and the merged design do not close Issue #305.
