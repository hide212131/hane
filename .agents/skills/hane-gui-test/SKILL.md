---
name: hane-gui-test
description: Verify native Hane GPUI changes with focused Rust GUI-event regression tests and macOS visual smoke tests. Use for PR or issue checks involving scrolling, clicking, focus, keyboard input, dialogs, or the work-folder sidebar in Hane; do not use for generic Rust-only testing.
---

# Hane GUI Test

Verify the requested behavior at the narrowest layer that can observe it, then add broader smoke-test evidence only where it improves confidence.

## Test strategy

1. Read the target diff and state the behavioral invariants before launching the app.
2. Run the relevant existing tests, then the workspace suite when proportional to the change. Run `cargo fmt --all -- --check` separately so an unrelated formatting failure does not prevent behavioral tests.
3. For hit-testing, event routing, focus, or internal viewport state, prefer a temporary `gpui::TestAppContext` regression test. Render the real `EditorView`, send a coordinate-specific event with `simulate_event`, and assert the affected model or view state.
4. Use Computer Use for visual appearance, native dialogs, and end-to-end smoke checks. Read the available `computer-use` skill before controlling macOS apps.
5. Report each invariant independently. Do not turn a visual-launch limitation into a failed product verdict when the behavior has been verified through GPUI's real render/event tree.

For GPUI event-test patterns and the macOS app-bundle fallback, read [references/gpui-macos-harness.md](references/gpui-macos-harness.md).

## Non-obvious constraints

- A binary started by `cargo run` can display correctly while remaining absent from Computer Use app discovery because it has no macOS bundle identifier. After one failed discovery check, package a fresh temporary `.app`; do not keep retrying names and paths.
- When startup arguments matter, use a native launcher executable in the bundle from its first registration. `open -a ... --args ...` may not reliably reach Hane, and replacing a registered bundle's main executable can leave stale LaunchServices state.
- GPUI exposes little useful accessibility content. Use the accessibility tree for native open panels and screenshots for Hane's custom-drawn UI.
- If coordinate actions return `noWindowsAvailable` after a fresh state read and one retry, stop spending time on Computer Use and switch to a coordinate-specific GPUI test.
- Creating or opening a temporary `.app` may require approval. Request it at the action point. Do not treat approval to launch the test app as permission for unrelated system changes.

## Cleanup and evidence

- Keep test fixtures under a unique temporary directory.
- If a regression test was added only for verification, remove exactly that test with `apply_patch` unless the user asks to retain it. Confirm `git status --short` is back to its initial state.
- Terminate only the test Hane process. Avoid broad process or filesystem cleanup targets.
- Report: branch/commit tested, behavioral invariants, targeted test result, workspace test result, visual smoke result, unrelated failures, and final worktree state.
