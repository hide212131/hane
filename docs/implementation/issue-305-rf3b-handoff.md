# Issue #305 implementation handoff

Updated 2026-10-17 from current GitHub state. This commit refreshes handoff evidence only; product code and tests remain untouched.

## Current state

- Issue #305 remains open. Current main is e82b9a9fe42280bfa78497746bcd7e21c73430c5.
- Design PR [#496](https://github.com/hide212131/hane/pull/496) is merged. Its final design head was 14def973e496c39b503c006db21fd187718ad683. CodeRabbit full review completed with no actionable findings. CI run [39002710437](https://github.com/hide212131/hane/actions/runs/39002710437) succeeded; macOS and Windows Rust jobs were skipped because the change was documentation-only.
- Comparing the design reading baseline 2164b895e849eef4de8c80c3696c0fd8e00c496b with current main shows documentation changes only; the product source used by the design is unchanged.
- This preparation PR [#497](https://github.com/hide212131/hane/pull/497) remains a draft and contains handoff documentation only. Its current head and workflow status are live GitHub facts and should be re-read when work begins. No product implementation has been produced.
- AADW model verification remains unresolved. Initial run [39004467591](https://github.com/hide212131/hane/actions/runs/39004467591) failed during model verification after the worker stage, before patch/finalizer. Diagnostic run [39004987391](https://github.com/hide212131/hane/actions/runs/39004987391) classified the issue as measurement, but did not confirm actual Sonnet 5.5 usage.
- User-authorized operations PR [#498](https://github.com/hide212131/hane/pull/498) has been merged at commit `e82b9a9fe42280bfa78497746bcd7e21c73430c5`, incorporating safe execution-file and modelUsage diagnostics into current main. Its exact reviewed head was `2be05561dfcb7c4758515e68c2c84aa463dccb22`; CI run [39005755218](https://github.com/hide212131/hane/actions/runs/39005755218) succeeded, including `Test Claude model verification`, while macOS and Windows Rust jobs were skipped for the `.github`-only change. At the user's direction, Codex review of that exact head found no actionable issues ([review](https://github.com/hide212131/hane/pull/498#pullrequestreview-5556939828)); CodeRabbit's manual full review remained rate-limited. This review and merge do not establish actual Sonnet 5.5 usage. The next step is one authorized AADW diagnostic on the current PR #497 head; do not request product implementation until the new run confirms usable execution-file diagnostics and model usage.

## Parallel PR #495

PR [#495](https://github.com/hide212131/hane/pull/495) is ready for review, open, and unmerged at current head `89e2f9d2c941948e82fb7a9ffc643bb757b8e5f7`; its 13-file diff overlaps this work in `crates/ui/src/actions.rs`, `crates/ui/src/view.rs`, `crates/ui/src/view/input_routing.rs`, `crates/ui/src/view/basic_editing_keys.rs`, and related tests. It introduces OS-standard editing keys, short-field document-edge selection, and input/IME protections.

The exact-head CI run [39015897230](https://github.com/hide212131/hane/actions/runs/39015897230) succeeded on macOS and Windows, including tests and clippy. The CodeRabbit review submission describes head `8d1248c600166aebec768d51bbf65db494a2e77d` and has one unresolved, non-outdated inline finding in `crates/ui/src/view.rs` about cancelling centered-caret mode before revealing a search match. #495 remains unaccepted. Do not import its product changes; if it is accepted before the #305 cutover is complete, re-read its then-current head and diff before continuing.

Do not import product changes from an unaccepted #495. If it remains unaccepted when Issue #305 implementation is authorized, implement against current main and preserve the documented OS-standard behavior around the overlap. If #495 is accepted before implementation starts, re-read its accepted head and diff, and include the conditional C09 check below.

## First implementation step

After an AADW diagnostic on current main confirms the execution log can be measured, continue the implementation on the existing PR #497. Before adding `ShortTextInput` or changing product behavior, re-read current main and #495's acceptance state, head, and diff. If #495 remains unaccepted, use current-main product code only. If #495 is accepted first, stop and reassess the overlap. Compare existing GPUI tests against accepted spec section 10.2, then add only the missing action/callback tests from C01-C08:

- C01: same-string replacement and empty deletion preserve the filter scroll reset and rename notification side effects; body content, revision, and history stay unchanged.
- C02: pending-rename Copy, Cut, Shift-arrows, SelectAll, Home/End, document-edge navigation/selection, Backspace/Delete, and Paste retain the entry-specific behavior in spec section 6.2.
- C03: both callbacks receiving CRLF are not treated as an empty OS cancel; verify the ordinary result after CR/LF removal.
- C04: a reversed selection followed by multiple mark updates and Windows-style or macOS-style cancel restores the original text, range, and direction without changing focus or the other input target.
- C05: filter and rename re-click behavior during composition, plus filter blur, remains unchanged.
- C06: deletion before a combining sequence or emoji is correct through the rename action route, with body text and file extension unchanged.
- C07: non-focused filter rendering hides only the painted selection, while the model selection remains; forward and reversed candidate-position queries use the moving end. Reuse the existing short-field caret-bounds test where it already covers the reversed case.
- C08: the late-body-cancel guard covers replace_text_in_range(Some(marked_range), "") and continues to block later non-empty callbacks.

If #495 is accepted before that work begins, also check C09: document-start/end actions select to the short field's edges, are no-ops for pending rename, and preserve Body document-edge behavior without reverting OS-specific bindings. Do not add C09 based on an unaccepted branch.

After these tests are in place, follow the accepted spec for the model, complete cutover of both fields, removal of duplicate state/helpers, and required verification. Do not add a synchronized old/new state period. Keep input routing, focus, pending rename, file operations, clipboard integration, rendering bounds, notifications, and list updates with their existing owners.

## Completion evidence and Issue state

The spec's M01-M15 unit tests, C01-C09 action/callback checks, and G01-G06 native GUI checks are plans, not completed evidence. The final implementation must meet the Issue acceptance criteria with current-head CI, review, and focused macOS/Windows GUI evidence. This handoff and the merged design do not close Issue #305.
