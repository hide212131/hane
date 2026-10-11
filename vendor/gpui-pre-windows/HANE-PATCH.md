# Hane Windows patch for GPUI 0.3.6

This directory vendors the crates.io package `gpui-pre-windows 0.3.6`, a
snapshot of Zed `bcf6582ce3500df93a8a39366640173e6786cea6`. The downloaded crate
SHA-256 is `27662e65bfcf4445d2cccfad91f1570ec07f4b1b5de98400a9f207fd19c464cf`.
The Apache-2.0 license is retained in `LICENSE-APACHE`. Hane changes only
`src/events.rs`, `src/vsync.rs`, and `src/dispatcher.rs`.

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

## Retry the main-thread wake notification after a failed `PostMessageW`

`WindowsDispatcher::dispatch_on_main_thread` gates the
`WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD` notification behind a `wake_posted`
flag so only the thread that flips it from false to true posts the wake
message; everyone else assumes a wake is already in flight. If `PostMessageW`
itself failed, the flag stayed stuck at true, so the runnable that was just
enqueued in `main_sender` (and any enqueued by another thread while the flag
was still true) could sit there with nobody ever posting the wake message
again. `WindowsPlatformState::run_foreground_task` in `src/platform.rs`
already resets this same flag to false when its own `PostMessageW` fails;
`dispatch_on_main_thread` now does the same, via a shared
`WindowsDispatcher::notify_main_thread` helper, so the next dispatch on any
thread retries the notification instead of leaving the queue silently stuck.

Relying solely on some later `dispatch_on_main_thread` call to retry the post
left a lost-wake interleaving: dispatch A claims `wake_posted` (false -> true)
and starts its `PostMessageW` call; dispatch B enqueues its own runnable while
A's post is in flight, observes `wake_posted == true`, and skips posting,
trusting A to wake the main thread; A's `PostMessageW` then fails and reset
the flag to false. Both runnables were left queued with no pending wake
message, and nothing guaranteed a later unrelated dispatch would ever arrive
to retry.

`notify_main_thread` now retries its own `PostMessageW` call synchronously,
up to `WindowsDispatcher::MAX_WAKE_POST_ATTEMPTS` (3) times, before giving up
the claim. This recovers the interleaving above without depending on a later
dispatch, while staying bounded so a persistently failing `PostMessageW`
(e.g. the window already being torn down) cannot turn into an unbounded
retry loop on the calling thread. If every attempt fails, the flag is still
released so a later dispatch can retry, matching the previous fallback
behavior.

Focused regression tests exercise the flag-reset/retry/skip logic, the
interleaved lost-wake scenario above, and the bounded give-up directly (they
do not call the real `PostMessageW`, since forcing that specific Win32
failure deterministically in a unit test is impractical). They are named
`hane_input_mode_notification_wake_retry_*` so the existing Windows CI test
command above (the one that selects the IME mode-notification and vsync
tests) also selects these dispatcher wake-retry tests: a lost main-thread
wake would stall delivery of the IME open/conversion-mode update just as
surely as a missing dispatch would.

```powershell
cargo test --manifest-path vendor/gpui-pre-windows/Cargo.toml --features test-support --lib hane_input_mode_notification
```
