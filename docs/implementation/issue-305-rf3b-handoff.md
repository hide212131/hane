# Issue #305 implementation handoff

Updated 2026-10-17 from current GitHub state. This commit refreshes handoff evidence only; product code and tests remain untouched.

## Current state

- Issue #305 remains open. Current main is e82b9a9fe42280bfa78497746bcd7e21c73430c5.
- Design PR [#496](https://github.com/hide212131/hane/pull/496) is merged. Its final design head was 14def973e496c39b503c006db21fd187718ad683. CodeRabbit full review completed with no actionable findings. CI run [39002710437](https://github.com/hide212131/hane/actions/runs/39002710437) succeeded; macOS and Windows Rust jobs were skipped because the change was documentation-only.
- Comparing the design reading baseline 2164b895e849eef4de8c80c3696c0fd8e00c496b with current main shows documentation changes only; the product source used by the design is unchanged.
- Implementation PR [#497](https://github.com/hide212131/hane/pull/497) remains open/Draft and currently contains only this handoff document; its live head and checks must be re-read before each request. Its base metadata still points to 21b9c85affeda2785145ec2052e1dcc9cb8e3ef1. Comparing current main e82b9a9fe42280bfa78497746bcd7e21c73430c5 with the PR branch shows only this handoff file in the PR-side diff; the two newer main commits are operations verification and Issue #460 documentation. No product code or tests have been changed.
- Initial implementation run [39004467591](https://github.com/hide212131/hane/actions/runs/39004467591) failed in trusted model verification after the worker, before patch/finalizer. Diagnostic run [39004987391](https://github.com/hide212131/hane/actions/runs/39004987391) classified the broad failure as measurement but could not identify whether the old execution file was missing, symlinked, oversized, or whether modelUsage was actually empty. The old raw execution file/artifact is unavailable, so the historical cause and model remain unconfirmed.
- User-authorized operations PR [#498](https://github.com/hide212131/hane/pull/498) is merged at e82b9a9fe42280bfa78497746bcd7e21c73430c5. Its exact reviewed head was 2be05561dfcb7c4758515e68c2c84aa463dccb22; CI run [39005755218](https://github.com/hide212131/hane/actions/runs/39005755218) succeeded, including Test Claude model verification, while Rust jobs were skipped for the .github-only change. At the user's direction, Codex review of that exact head found no actionable findings ([review](https://github.com/hide212131/hane/pull/498#pullrequestreview-5556939828)); this does not mean CodeRabbit reviewed it. Current-main diagnostic run [39017012794](https://github.com/hide212131/hane/actions/runs/39017012794), targeting the then-current PR #497 head, completed end to end. The trusted verifier observed a bounded regular, non-symlink execution file (56,437 bytes) and positive claude-sonnet-5-5 modelUsage; prepare, worker, verifier, unchanged-source validation, and artifact upload succeeded. Its report classifies the historical failure as unknown; it confirms the current measurement path works, not what happened in the old run. An earlier malformed marker attempt (run [39016952069](https://github.com/hide212131/hane/actions/runs/39016952069)) failed before the worker and made no changes.

## Parallel PR #495

PR [#495](https://github.com/hide212131/hane/pull/495) is ready for review, open, and unmerged at current head `89e2f9d2c941948e82fb7a9ffc643bb757b8e5f7`; its 13-file diff overlaps this work in `crates/ui/src/actions.rs`, `crates/ui/src/view.rs`, `crates/ui/src/view/input_routing.rs`, `crates/ui/src/view/basic_editing_keys.rs`, and related tests. It introduces OS-standard editing keys, short-field document-edge selection, and input/IME protections.

The exact-head CI run [39015897230](https://github.com/hide212131/hane/actions/runs/39015897230) succeeded on macOS and Windows, including tests and clippy. The CodeRabbit review submission describes head `8d1248c600166aebec768d51bbf65db494a2e77d` and has one unresolved, non-outdated inline finding in `crates/ui/src/view.rs` about cancelling centered-caret mode before revealing a search match. #495 remains unaccepted. Do not import its product changes; if it is accepted before the #305 cutover is complete, re-read its then-current head and diff before continuing.

Do not import product changes from an unaccepted #495. If it remains unaccepted when Issue #305 implementation is authorized, implement against current main and preserve the documented OS-standard behavior around the overlap. If #495 is accepted before implementation starts, re-read its accepted head and diff, and include the conditional C09 check below.

## First implementation step

The current-main diagnostic is complete: run 39017012794 confirmed that the trusted verifier can measure a regular execution file and positive Sonnet 5.5 usage. The user has authorized continuing through implementation and acceptance. Before any product change, re-read current main and #495's acceptance state, head, and diff. If #495 is still unaccepted, use current-main product code only and do not import its branch; if #495 has been accepted, stop and reassess the overlap. First compare existing GPUI tests with accepted spec section 10.2, then make one tests-only request for missing C01-C08 coverage. Keep ShortTextInput and all production behavior unchanged in that first request. Require its focused tests and current-head CI to pass before a separate implementation request for the model, complete cutover of both fields, and removal of duplicate state/helpers.

- C01: same-string replacement and empty deletion preserve the filter scroll reset and rename notification side effects; body content, revision, and history stay unchanged.
- C02: pending-rename Copy, Cut, Shift-arrows, SelectAll, Home/End, document-edge navigation/selection, Backspace/Delete, and Paste retain the entry-specific behavior in spec section 6.2.
- C03: both callbacks receiving CRLF are not treated as an empty OS cancel; verify the ordinary result after CR/LF removal.
- C04: a reversed selection followed by multiple mark updates and Windows-style or macOS-style cancel restores the original text, range, and direction without changing focus or the other input target.
- C05: filter and rename re-click behavior during composition, plus filter blur, remains unchanged.
- C06: deletion before a combining sequence or emoji is correct through the rename action route, with body text and file extension unchanged.
- C07: non-focused filter rendering hides only the painted selection, while the model selection remains; forward and reversed candidate-position queries use the moving end. Reuse the existing short-field caret-bounds test where it already covers the reversed case.
- C08: the late-body-cancel guard covers replace_text_in_range(Some(marked_range), "") and continues to block later non-empty callbacks.

If #495 is accepted before that work begins, also check C09: document-start/end actions select to the short field's edges, are no-ops for pending rename, and preserve Body document-edge behavior without reverting OS-specific bindings. Do not add C09 based on an unaccepted branch.

After the tests-only change is verified, re-check #495 and current main. If #495 remains unaccepted, use the current-main code path only. Then make a separate implementation request that follows the accepted spec for the model, complete cutover of both fields, removal of duplicate state/helpers, and required verification. Do not add a synchronized old/new state period. Keep input routing, focus, pending rename, file operations, clipboard integration, rendering bounds, notifications, and list updates with their existing owners.

## Completion evidence and Issue state

The spec's M01-M15 unit tests, C01-C09 action/callback checks, and G01-G06 native GUI checks are plans, not completed evidence. The final implementation must meet the Issue acceptance criteria with current-head CI, review, and focused macOS/Windows GUI evidence. This handoff and the merged design do not close Issue #305.
