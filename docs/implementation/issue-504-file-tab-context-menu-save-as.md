# Issue #504: file-tab context menu Save As

- Issue: https://github.com/hide212131/hane/issues/504
- Phase: implementation
- Preparation base (current main): `e82b9a9fe42280bfa78497746bcd7e21c73430c5`
- Status at preparation: implementation not started; tests and GUI validation not run

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

## Verification needed

Run the repository-required tests and lint on the exact resulting head. Perform the focused macOS GUI scenario for a new edited note, including arbitrary destination and cancellation, and record evidence against the final head/base.
