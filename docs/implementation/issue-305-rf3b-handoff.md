# Issue #305 implementation handoff

## Current state

- Issue #305 is open. The accepted design is available on `main` at `21b9c85affeda2785145ec2052e1dcc9cb8e3ef1`.
- Design PR [#496](https://github.com/hide212131/hane/pull/496) was merged as a documentation-only change.
- The design source of truth is [`docs/refactor-rf3b-short-text-input-implementation-spec.md`](../refactor-rf3b-short-text-input-implementation-spec.md).
- This preparation commit contains no product implementation. Keep Issue #305 open and its product acceptance criteria unchecked until implementation and required verification are complete.

## Goal and boundaries

Implement the accepted design by introducing the small concrete `ShortTextInput` model in `crates/ui/src/short_text_input.rs`, then moving filter and rename to that shared model and removing their duplicate state/helpers.

Follow the accepted spec for fields, update APIs, helper ownership, file list, fixtures, Unicode examples, migration sequence, rollback, and verification. Keep input-target routing, pending state, file operations, clipboard integration, focus, rendering/caret bounds, notifications, and list updates with their existing owners. Do not turn the body editor into this model or introduce another general IME framework.

## First implementation step

Before editing product code, inspect the current `main` implementation and compare the relevant routing and selection changes with the latest state of PR #495. Then add focused tests for existing filter/rename behavior gaps identified by the accepted spec. Continue the implementation in this same PR only after those tests are in place.

## Parallel PR #495

At handoff preparation, PR #495 is Draft/open at head `22edc0ed1ce77834eb9cb12474bba61e7451a0be`. It overlaps in short-field document-edge selection and input/IME routing. Its current-head CI run [38996263320](https://github.com/hide212131/hane/actions/runs/38996263320) is failed on macOS and Windows at `a_wheel_scroll_after_center_cursor_line_is_not_pulled_back`.

Do not import or rely on unaccepted #495 product changes. If #495 is accepted during this work, inspect its then-current head, diff, acceptance, and CI again, and preserve the behavior the accepted spec assigns to filter and rename while reconciling only the overlapping routing/state/tests.

## Completion evidence

Complete the implementation and migration described by the accepted spec, remove obsolete duplicate state, and provide the required Rust tests and clippy results plus focused macOS and Windows native GUI evidence for input, IME/composition, and candidate positioning. Follow repository CI and review policy. Report any unmet acceptance criterion explicitly; do not close Issue #305 from the design or preparation stage.
