# GPUI and macOS test harness

Read the section matching the current blocker. Prefer the GPUI route for behavioral assertions and the macOS route for visual smoke checks.

## Coordinate-specific GPUI events

Use this when the result depends on which region receives a mouse, wheel, or keyboard event. It exercises GPUI's real layout, hit-testing, capture/bubble routing, and listener placement without depending on macOS automation.

Typical shape inside `crates/ui/src/view.rs` tests:

```rust
#[gpui::test]
fn sidebar_wheel_does_not_scroll_the_editor(cx: &mut gpui::TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, cx| EditorView::open_work_folder(&root, cx));
    cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
    cx.run_until_parked();

    let before = view.read_with(cx, |view, _| view.scroll_y);
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: gpui::point(px(100.0), px(300.0)), // sidebar
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-120.0))),
        ..Default::default()
    });
    assert_eq!(view.read_with(cx, |view, _| view.scroll_y), before);

    cx.simulate_event(gpui::ScrollWheelEvent {
        position: gpui::point(px(500.0), px(300.0)), // editor
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-120.0))),
        ..Default::default()
    });
    assert!(view.read_with(cx, |view, _| view.scroll_y) > before);
}
```

Create enough content for both target regions to scroll. Use `add_window_view`, set a deterministic window size, wait for background work, and assert state rather than painted text. Run the filter without `--exact` unless using the fully qualified name such as `view::tests::sidebar_wheel_does_not_scroll_the_editor`.

For a temporary test, capture the original surrounding lines before patching so removal restores the file byte-for-byte. Check `git diff --check` and `git status --short` afterward.

## Making Hane discoverable to Computer Use

Computer Use selects a macOS application by bundle identity. A bare `target/debug/hane` process may not appear even though its window is visible.

Use a unique temporary directory and create this structure:

```text
HaneGuiTest.app/
└── Contents/
    ├── Info.plist
    └── MacOS/
        ├── hane-launcher
        └── hane-bin
```

`Info.plist` needs at least `CFBundleExecutable`, a unique `CFBundleIdentifier`, `CFBundleName`, and `CFBundlePackageType` set to `APPL`. Copy the current `target/debug/hane` to `hane-bin`.

When Hane must start with a file or work folder, compile a tiny native launcher before the bundle is opened. The launcher should `chdir` to the repository and `execl` `hane-bin` with the absolute target path. Make `CFBundleExecutable` name the launcher. This avoids both the unreliable `open --args` path and native open-panel automation.

Open the fresh bundle with `open -a /absolute/path/HaneGuiTest.app`. This is a GUI launch and may require approval. After approval, use `sky.list_apps()` once to obtain the actual bundle id, then `sky.get_app_state()`.

Do not replace the main executable after LaunchServices has registered the bundle. A script launcher can produce `kLSNoExecutableErr`, and replacing it with a Mach-O binary may still leave cached invalid state. Create a new bundle path and bundle identifier instead.

## Computer Use stopping rule

Hane's GPUI content may expose only the standard window controls in the accessibility tree. That is expected; inspect the screenshot.

If a coordinate click or scroll reports `noWindowsAvailable`:

1. Read fresh app state using the discovered bundle id.
2. Retry the intended coordinate action once.
3. If it fails again, record the automation limitation and use a GPUI `simulate_event` regression test for the behavior.

Do not rebuild multiple bundles merely to overcome coordinate automation when GPUI can directly verify the invariant.
