# Issue #504: file-tab context menu Save As

- Issue: https://github.com/hide212131/hane/issues/504
- Phase: implementation
- Preparation base (current main): `e82b9a9fe42280bfa78497746bcd7e21c73430c5`
- Status at preparation (historical): implementation not started; tests and GUI validation not run. See "Verification status" for the current state.

## Goal

After editing a new note, let the user right-click its file tab and choose an arbitrary Save As destination, including outside the Work folder. The action must apply to the tab that opened the menu, including when that tab is not active.

## Implementation scope

- Add Save As to the existing file-tab context menu.
- Bind the native save response to the clicked tab's session/document instance so a tab switch or replacement while the dialog is open cannot save the wrong document.
- Reuse the existing Save As, atomic write, and draft recovery flows. Do not add a second save/draft state machine.
- On success, update the tab, open session, and sidebar to the chosen destination.
- Preserve unsaved content and its recovery draft on cancellation or failure; rely on the native dialog's overwrite confirmation.
- Keep the sidebar-row context menu in Issue #236 / PR #501. Preserve the existing file-tab menu actions. Do not change the separate save/draft refactors.

## Acceptance criteria for this phase

- A file tab exposes Save As from its right-click menu.
- A newly created and edited note can be saved outside the Work folder from the tab menu.
- With multiple tabs open, Save As targets the tab that was right-clicked.
- Cancel, failure, or a tab/document change during the native dialog cannot discard or misdirect the edited content.
- Existing file-tab context-menu actions continue to work.

## Implementation note

- The file-tab menu gained `file-tab-context-save-as` ("名前を付けて保存…"). It is bound to the menu's session id and document instance and calls the existing `prompt_save_as_for`, so the answer flows through `handle_save_as_response` and `save_session` (atomic write, draft retirement, sidebar indexing). A stale menu instance opens no dialog.
- Regression tests were added in `crates/ui/src/view.rs`.

## Verification status

Completed on exact head `30896d8d47171ca32f5b89b67151fdc4f114e5d2`:

- The macOS and Windows workspace tests and Clippy succeeded in CI run `39032368476`. The added regression tests are included in those workspace tests.
- In an isolated macOS app, Cmd+Shift+S was used to choose a temporary destination outside the Work folder, and the saved content was read back. This confirms the save dialog / save path only; it is not a confirmation of the tab right-click operation.
- The GPUI real-event test `right_clicking_a_file_tab_targets_that_session_without_activating_it` confirms that the Save As item is shown in the file-tab menu.

Still unconfirmed:

- Showing the tab right-click menu on a real screen and the cancel behavior from that menu. Perform the focused macOS GUI scenario for a new edited note (arbitrary destination and cancellation) and record evidence against the final head/base.
