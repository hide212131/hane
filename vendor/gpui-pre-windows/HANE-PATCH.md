# Hane Windows patch for GPUI 0.3.6

This directory vendors the crates.io package `gpui-pre-windows 0.3.6`, a
snapshot of Zed `bcf6582ce3500df93a8a39366640173e6786cea6`. The downloaded crate
SHA-256 is `27662e65bfcf4445d2cccfad91f1570ec07f4b1b5de98400a9f207fd19c464cf`.
The Apache-2.0 license is retained in `LICENSE-APACHE`. Hane changes only
`src/events.rs` and `src/vsync.rs`.

## Refresh the caret's input-mode badge

The GPUI 0.3.6 Windows message handler refreshes keyboard-layout subscribers on
`WM_INPUTLANGCHANGE`, but a Japanese IME can switch between ASCII and native
input without changing the keyboard layout. The Windows message handler now
routes `WM_IME_NOTIFY` notifications for `IMN_SETOPENSTATUS` and
`IMN_SETCONVERSIONMODE` through GPUI's existing keyboard-layout refresh path.
When the window becomes active, it also requests that refresh so Hane reads the
current mode after returning from another application.

The focused regression tests verify that both IME mode notifications dispatch
the refresh and unrelated notifications do not:

```powershell
cargo test --manifest-path vendor/gpui-pre-windows/Cargo.toml --features test-support --lib hane_input_mode_notification
```

The same Windows CI test command also selects the `src/vsync.rs` regression
tests. They verify that DWM timing conversion preserves sub-millisecond
precision, rejects a zero denominator and intervals shorter than 1 ms, and
accepts the 1 ms boundary. Invalid timing values return an error so
`VSyncProvider::new` uses `DEFAULT_VSYNC_INTERVAL`.

The Hane UI tests for mode changes, repeated values, and unknown state run with
the normal workspace test command. These tests do not replace the required
Windows 11 + Microsoft IME GUI check of `A → あ → A` on the exact final PR head.
