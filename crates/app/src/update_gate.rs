//! The application's startup gate (Issue #510, U4 increment 1).
//!
//! `main` calls [`enter`] before anything else: before the AI service, the
//! state stores, a window or any open request exists. While a pending update
//! transaction is being settled, or if it cannot be settled, the process never
//! reaches normal editing.

use std::path::PathBuf;

use hane_update::health::{COMMIT_WAIT, ChildEntry, VerifiedChild, detect_child};
use hane_update::startup::{
    GateOutcome, HelperRecoveryRunner, RuntimeGuard, STARTUP_WAIT, startup_gate_for_exe_within,
};
use hane_update::trust::TrustPolicy;

/// Exit status of a launch that must not start the editor (EX_TEMPFAIL / EX_SOFTWARE).
const EXIT_UPDATE_IN_PROGRESS: i32 = 75;
const EXIT_RECOVERY_REQUIRED: i32 = 70;

pub enum Entry {
    /// Start normally. Hold the guard until the process ends: it is the
    /// shared runtime lease that tells a helper this process is still alive.
    /// `state_root` is set only for a committed update's child: the root the
    /// updating application used and the child verified read-only, which the
    /// normal start must use instead of resolving one from its own (empty)
    /// environment.
    Start {
        guard: Option<RuntimeGuard>,
        state_root: Option<PathBuf>,
    },
    /// The limited-health child of an update helper. Only the minimal update
    /// UI may start; `Healthy` follows its first drawn frame and normal startup
    /// follows the helper's durable `Commit`.
    Limited(Box<VerifiedChild>),
    /// Do not start; exit the process with this status.
    Stop(i32),
}

/// How long the limited child waits for the helper's `Commit` after `Healthy`.
pub const LIMITED_COMMIT_WAIT: std::time::Duration = COMMIT_WAIT;

/// How long a launch carrying an explicit file argument keeps waiting for an
/// update in progress, so the request is delivered instead of dropped.
const EXPLICIT_OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const EXPLICIT_OPEN_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Takes the startup gate. With an open request, `UpdateInProgress` is retried
/// (each attempt takes the gate again). `wait` is the total real-time budget,
/// including the time spent inside the gate calls; `None` means no budget: the
/// gate is retaken until it reaches a terminal outcome (a Finder event is
/// delivered only once by LaunchServices, so the process must outlive any
/// update). Every other outcome is returned at once. `elapsed` reports the time
/// since the start, and each attempt is told how long it may wait for the
/// startup lease: never more than the remaining budget.
fn gate_with_open_retry(
    mut take_gate: impl FnMut(std::time::Duration) -> GateOutcome,
    has_explicit_open: bool,
    wait: Option<std::time::Duration>,
    mut sleep: impl FnMut(std::time::Duration),
    mut elapsed: impl FnMut() -> std::time::Duration,
) -> GateOutcome {
    loop {
        let attempt_wait = match wait {
            Some(wait) if has_explicit_open => wait.saturating_sub(elapsed()).min(STARTUP_WAIT),
            _ => STARTUP_WAIT,
        };
        let outcome = take_gate(attempt_wait);
        if !has_explicit_open || !matches!(outcome, GateOutcome::UpdateInProgress) {
            return outcome;
        }
        let pause = match wait {
            Some(wait) => {
                let remaining = wait.saturating_sub(elapsed());
                if remaining.is_zero() {
                    return outcome;
                }
                EXPLICIT_OPEN_RETRY_INTERVAL.min(remaining)
            }
            None => EXPLICIT_OPEN_RETRY_INTERVAL,
        };
        sleep(pause);
    }
}

/// The retry budget for a launch that may carry an open request. A macOS launch
/// may receive a Finder document that LaunchServices delivers only once, so it
/// has no time limit and ends only on a terminal gate outcome; a launch with
/// just a command-line path keeps the bounded wait on Linux. A Windows launch
/// with a command-line path is likewise unbounded: the one-shot request is held
/// until the gate reaches a terminal outcome instead of expiring after 60s.
fn open_request_wait(has_cli_path: bool) -> Option<std::time::Duration> {
    open_request_wait_for(
        has_cli_path,
        cfg!(target_os = "macos"),
        cfg!(target_os = "windows"),
    )
}

fn open_request_wait_for(
    has_cli_path: bool,
    is_macos: bool,
    is_windows: bool,
) -> Option<std::time::Duration> {
    if is_macos || (is_windows && has_cli_path) || !has_cli_path {
        None
    } else {
        Some(EXPLICIT_OPEN_WAIT)
    }
}

/// Whether this launch may carry a document to open. On macOS, LaunchServices
/// delivers Finder documents through `on_open_urls` rather than argv, and only
/// once the application runs, which is after this gate. Such a launch cannot be
/// told apart from an ordinary one, so every macOS launch waits out an update in
/// progress: the pending Finder event stays with the OS until the gate admits
/// normal startup and is then queued by the callback registered before `run`
/// and opened once through the normal open flow. The gate itself is never
/// bypassed, and `RecoveryRequired` still stops the launch.
fn may_carry_open_request(has_cli_path: bool) -> bool {
    has_cli_path || cfg!(target_os = "macos")
}

/// Whether the first argument is an actual file path to open. The context menu
/// management flags carry no open request, so they keep the bounded gate.
fn is_cli_open_path(first_arg: Option<&std::ffi::OsStr>) -> bool {
    match first_arg {
        None => false,
        Some(arg) => arg != "--register-context-menu" && arg != "--unregister-context-menu",
    }
}

/// Whether a gate that admits normal startup must first be re-executed. A macOS
/// launch that observed `UpdateInProgress` waited while the bundle was swapped,
/// so its in-memory image may be the pre-update version; it must never go on to
/// edit documents or state. Terminal stops are returned unchanged.
fn needs_fresh_image(observed_update: bool, outcome: &GateOutcome, is_macos: bool) -> bool {
    is_macos
        && observed_update
        && matches!(outcome, GateOutcome::Proceed(_) | GateOutcome::Unmanaged)
}

/// Set on the re-executed image so a still-mismatching disk cannot loop.
const REEXEC_MARKER_ENV: &str = "HANE_UPDATE_GATE_REEXEC";

/// Identity of this build's executable. It embeds the version, which every
/// update changes; the same bytes are in the on-disk executable of the same
/// build and absent from any other version's.
#[used]
static IMAGE_MARKER: &str = concat!("hane-image-generation=", env!("CARGO_PKG_VERSION"), ";");

fn image_marker() -> &'static [u8] {
    std::hint::black_box(IMAGE_MARKER).as_bytes()
}

/// Whether `reader` contains `needle`, read in bounded chunks.
fn reader_contains(mut reader: impl std::io::Read, needle: &[u8]) -> std::io::Result<bool> {
    if needle.is_empty() {
        return Ok(true);
    }
    let mut buf = vec![0u8; 1 << 20];
    let mut window: Vec<u8> = Vec::new();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(false);
        }
        window.extend_from_slice(&buf[..n]);
        if window.windows(needle.len()).any(|w| w == needle) {
            return Ok(true);
        }
        let keep = needle.len() - 1;
        if window.len() > keep {
            window.drain(..window.len() - keep);
        }
    }
}

/// Whether the executable at `exe` is the same build as the running image.
/// An unreadable file counts as different: the caller then restarts or stops.
fn disk_image_is_running_image(exe: &std::path::Path) -> bool {
    match std::fs::File::open(exe) {
        Ok(file) => reader_contains(file, image_marker()).unwrap_or(false),
        Err(_) => false,
    }
}

/// Takes the gate so that any wait for a held lease is observable here. The
/// gate waits for the startup lease internally and may then return `Proceed`
/// without ever reporting `UpdateInProgress`, which would hide that this image
/// outlived an update. A non-waiting probe comes first: if the lease is held,
/// that is recorded in `observed_update` before the waiting attempt is made.
fn take_gate_observing_waits(
    mut take_gate: impl FnMut(std::time::Duration) -> GateOutcome,
    wait: std::time::Duration,
    observed_update: &std::cell::Cell<bool>,
) -> GateOutcome {
    let probe = take_gate(std::time::Duration::ZERO);
    if !matches!(probe, GateOutcome::UpdateInProgress) {
        return probe;
    }
    observed_update.set(true);
    take_gate(wait)
}

/// Whether the on-disk executable may be re-executed. A version marker mismatch
/// or an unreadable file alone never vouches for it: the bundle must verify
/// against the pinned Team ID (Developer ID signature) and be accepted by
/// Gatekeeper as notarized. `verify` performs that check; without a pinned Team
/// ID nothing can be proven and the answer is a refusal.
#[cfg(any(target_os = "macos", test))]
fn reexec_trust_decision(
    team_id: Option<&str>,
    verify: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    let team_id = team_id.ok_or_else(|| {
        "実行ファイルの署名を確認するTeam IDが設定されていないため、再起動しません".to_string()
    })?;
    verify(team_id).map_err(|reason| {
        format!("ディスク上の実行ファイルの署名・配置を確認できないため、再起動しません（{reason}）")
    })
}

/// Verifies, with the existing trust policy, that the bundle containing `exe`
/// is a Developer ID signed (pinned Team ID), Gatekeeper-accepted install.
#[cfg(target_os = "macos")]
fn verify_exe_for_reexec(exe: &std::path::Path, policy: &TrustPolicy) -> Result<(), String> {
    use hane_update::macos_apply::BundleHost;
    use hane_update::platform::macos::{MacHost, locate, verify_developer_id};
    reexec_trust_decision(policy.macos_team_id(), |team_id| {
        let placement = locate(exe).map_err(|err| err.to_string())?;
        verify_developer_id(&placement.install_root, team_id).map_err(|err| err.to_string())?;
        MacHost
            .assess_gatekeeper(&placement.install_root)
            .map_err(|err| err.to_string())
    })
}

#[cfg(not(target_os = "macos"))]
fn verify_exe_for_reexec(_exe: &std::path::Path, _policy: &TrustPolicy) -> Result<(), String> {
    Err("この環境では実行ファイルの署名を確認できません".to_string())
}

/// Replaces this process with the current on-disk executable and the original
/// arguments. Only returns on failure, with the reason.
#[cfg(unix)]
fn reexec_current_image(exe: &std::path::Path) -> String {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(REEXEC_MARKER_ENV, "1")
        .exec();
    err.to_string()
}

#[cfg(not(unix))]
fn reexec_current_image(_exe: &std::path::Path) -> String {
    "re-execution is not supported on this platform".to_string()
}

/// A stop message that must reach the person who launched Hane. A Finder launch
/// has no terminal, and the GUI is not initialised yet, so stderr alone is never
/// seen.
#[derive(Debug, PartialEq, Eq)]
struct StopNotice {
    title: String,
    message: String,
}

/// The notice for a failed restart into the updated executable. It names the
/// reason, says the request made during the update (a plain launch or a
/// document to open) was not handled, and tells the user to start Hane again.
fn reexec_failure_notice(reason: &str) -> StopNotice {
    StopNotice {
        title: "Haneを更新後の状態で再起動できませんでした".to_string(),
        message: format!(
            "更新中に行った起動操作や文書を開く操作は処理されませんでした。\
             文書・設定・AIのデータには変更を加えていません。\n\n\
             理由: {reason}\n\n\
             更新は完了しています。Haneを終了した状態で、もう一度Haneを起動し、\
             開きたい文書があれば開き直してください。"
        ),
    }
}

/// `osascript` arguments that show `notice` as an alert. The text is passed as
/// script arguments, never spliced into the script source.
#[cfg(any(target_os = "macos", test))]
fn osascript_alert_args(notice: &StopNotice) -> Vec<String> {
    vec![
        "-e".to_string(),
        "on run argv".to_string(),
        "-e".to_string(),
        "display alert (item 1 of argv) message (item 2 of argv) as critical".to_string(),
        "-e".to_string(),
        "end run".to_string(),
        "--".to_string(),
        notice.title.clone(),
        notice.message.clone(),
    ]
}

/// The notice for a launch that cannot be handled while an update is applied.
fn update_in_progress_notice() -> StopNotice {
    StopNotice {
        title: "Haneは更新中です".to_string(),
        message: "更新中のため、起動操作や文書を開く操作を処理できませんでした。\
                  文書・設定・AIのデータには変更を加えていません。\n\n\
                  更新が終わってから、もう一度Haneを起動するか、開きたい文書を開き直してください。"
            .to_string(),
    }
}

/// The notice for a launch stopped because an interrupted update is not settled.
/// It does not claim the update succeeded.
fn recovery_required_notice(reason: &str) -> StopNotice {
    StopNotice {
        title: "Haneの更新を完了または取り消せませんでした".to_string(),
        message: format!(
            "中断された更新の復旧が済んでいないため、起動操作や文書を開く操作を処理できませんでした。\
             文書・設定・AIのデータには変更を加えていません。\n\n\
             理由: {reason}\n\n\
             署名済みのHane ZIPを手動でインストールするなど、復旧の案内に従ってください。\
             復旧が済んだ後で、もう一度Haneを起動するか、開きたい文書を開き直してください。"
        ),
    }
}

/// Where a stop notice is shown on a platform.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum NoticeSurface {
    MacosAlert,
    WindowsMessageBox,
    StderrOnly,
}

fn notice_surface(is_macos: bool, is_windows: bool) -> NoticeSurface {
    if is_macos {
        NoticeSurface::MacosAlert
    } else if is_windows {
        NoticeSurface::WindowsMessageBox
    } else {
        NoticeSurface::StderrOnly
    }
}

/// UTF-16, NUL-terminated, as `MessageBoxW` expects.
#[cfg(any(target_os = "windows", test))]
fn to_wide_nul(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Shows the notice in a native dialog (blocking until dismissed, so the
/// process does not exit before it is read): an alert on macOS and a message
/// box on Windows, whose GUI build has no console. Other platforms keep stderr.
fn present_notice(notice: &StopNotice) {
    eprintln!("{}\n{}", notice.title, notice.message);
    match notice_surface(cfg!(target_os = "macos"), cfg!(target_os = "windows")) {
        NoticeSurface::MacosAlert => {
            #[cfg(target_os = "macos")]
            {
                let _ = std::process::Command::new("/usr/bin/osascript")
                    .args(osascript_alert_args(notice))
                    .status();
            }
        }
        NoticeSurface::WindowsMessageBox => {
            #[cfg(target_os = "windows")]
            {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
                };
                let title = to_wide_nul(&notice.title);
                let message = to_wide_nul(&notice.message);
                // SAFETY: both buffers are NUL-terminated and outlive the call.
                unsafe {
                    MessageBoxW(
                        std::ptr::null_mut(),
                        message.as_ptr(),
                        title.as_ptr(),
                        MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
                    );
                }
            }
        }
        NoticeSurface::StderrOnly => {}
    }
}

/// Reports a recovery-required stop through `present` and stops.
fn stop_for_recovery(reason: &str, present: impl FnOnce(&StopNotice)) -> Entry {
    present(&recovery_required_notice(reason));
    Entry::Stop(EXIT_RECOVERY_REQUIRED)
}

/// Turns a terminal gate refusal into a visible notice and a stop. Admitting
/// outcomes are not handled here.
fn stop_for_refusal(outcome: GateOutcome, present: impl FnOnce(&StopNotice)) -> Entry {
    match outcome {
        GateOutcome::Proceed(guard) => Entry::Start {
            guard: Some(guard),
            state_root: None,
        },
        GateOutcome::Unmanaged => Entry::Start {
            guard: None,
            state_root: None,
        },
        GateOutcome::UpdateInProgress => {
            present(&update_in_progress_notice());
            Entry::Stop(EXIT_UPDATE_IN_PROGRESS)
        }
        GateOutcome::RecoveryRequired(reason) => stop_for_recovery(&reason, present),
    }
}

/// Fails closed after a failed re-exec: reports through `present` and stops.
fn stop_after_failed_reexec(reason: &str, present: impl FnOnce(&StopNotice)) -> Entry {
    present(&reexec_failure_notice(reason));
    Entry::Stop(EXIT_UPDATE_IN_PROGRESS)
}

pub fn enter() -> Entry {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            // The placement cannot be resolved, so it cannot be ruled out that
            // an interrupted update is pending: do not start normal editing.
            return stop_for_recovery(
                &format!("実行中の実行ファイルの場所を特定できません: {err}"),
                present_notice,
            );
        }
    };
    // The limited-health child of an update helper is the only launch that may
    // pass the gate without taking it, and only through the private capability
    // and pipes (never a flag or an environment variable). It starts nothing
    // else until the helper has made the commit durable.
    match detect_child(&exe, env!("CARGO_PKG_VERSION")) {
        ChildEntry::NotChild => {}
        ChildEntry::Refused(reason) => {
            return stop_for_recovery(
                &format!("起動対象の更新を確認できません: {reason}"),
                present_notice,
            );
        }
        ChildEntry::Child(session) => {
            // The read-only inspection of the state root, the settings store
            // and the restart ticket runs before `Healthy`; it writes nothing.
            // The root is handed on only if the helper's `Commit` arrives.
            // `Healthy` is not sent here: the limited UI must first draw its
            // first frame (see `limited_start`).
            let check = session.state_check();
            return match session.verify(move || check.verify()) {
                Ok(verified) => Entry::Limited(Box::new(verified)),
                Err(err) => {
                    stop_for_recovery(
                        &format!("起動対象の更新を確認できません: {err}"),
                        present_notice,
                    )
                }
            };
        }
    }
    let runner = HelperRecoveryRunner {
        policy: TrustPolicy::production(),
    };
    // An explicit file argument must not be lost to an update in progress: keep
    // taking the gate (never bypassing it) and let this process deliver the path
    // through the ordinary open path once the gate admits it.
    let has_cli_path = is_cli_open_path(std::env::args_os().nth(1).as_deref());
    let has_explicit_open = may_carry_open_request(has_cli_path);
    let started = std::time::Instant::now();
    let observed_update = std::cell::Cell::new(false);
    let outcome = gate_with_open_retry(
        |wait| {
            let outcome = take_gate_observing_waits(
                |attempt| startup_gate_for_exe_within(&exe, &runner, attempt),
                wait,
                &observed_update,
            );
            if matches!(outcome, GateOutcome::UpdateInProgress) {
                observed_update.set(true);
            }
            outcome
        },
        has_explicit_open,
        open_request_wait(has_cli_path),
        std::thread::sleep,
        || started.elapsed(),
    );
    // A momentary lease observation cannot prove that no update happened (the
    // helper may release the lease before the probe), so on macOS the running
    // image is also compared with the signed executable now on disk.
    let disk_differs = cfg!(target_os = "macos")
        && matches!(outcome, GateOutcome::Proceed(_) | GateOutcome::Unmanaged)
        && !disk_image_is_running_image(&exe);
    if needs_fresh_image(
        observed_update.get() || disk_differs,
        &outcome,
        cfg!(target_os = "macos"),
    ) {
        // This process image was started before the update and may be the old
        // version. Release the runtime lease, then replace the image in place
        // (same PID, so LaunchServices still delivers the pending Finder event
        // once, to the new image) with the current on-disk executable. If that
        // cannot be done, fail closed rather than edit with the old image.
        drop(outcome);
        if disk_differs && std::env::var_os(REEXEC_MARKER_ENV).is_some() {
            // Already restarted once and the disk still differs: never loop.
            return stop_after_failed_reexec(
                "再起動後も、実行中の版とディスク上の実行ファイルが一致しません",
                present_notice,
            );
        }
        // The file is run only if it is proven to be a signed, accepted install.
        if let Err(reason) = verify_exe_for_reexec(&exe, &TrustPolicy::production()) {
            return stop_after_failed_reexec(&reason, present_notice);
        }
        let err = reexec_current_image(&exe);
        return stop_after_failed_reexec(&err, present_notice);
    }
    stop_for_refusal(outcome, present_notice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn scripted(in_progress: usize) -> impl FnMut(Duration) -> GateOutcome {
        let mut remaining = in_progress;
        move |_| {
            if remaining > 0 {
                remaining -= 1;
                GateOutcome::UpdateInProgress
            } else {
                GateOutcome::Unmanaged
            }
        }
    }

    #[test]
    fn an_explicit_open_waits_out_the_update_and_is_admitted_once() {
        let mut admitted = 0;
        let mut inner = scripted(3);
        let mut sleeps = 0;
        let outcome = gate_with_open_retry(
            |wait| {
                let outcome = inner(wait);
                if matches!(outcome, GateOutcome::Unmanaged) {
                    admitted += 1;
                }
                outcome
            },
            true,
            Some(Duration::from_secs(60)),
            |_| sleeps += 1,
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert_eq!(admitted, 1, "the launch (and its path) is admitted exactly once");
        assert_eq!(sleeps, 3);
    }

    #[test]
    fn an_old_image_that_waited_for_an_update_does_not_proceed_in_place() {
        let observed = std::cell::Cell::new(false);
        let mut inner = scripted(2);
        let outcome = gate_with_open_retry(
            |wait| {
                let outcome = inner(wait);
                if matches!(outcome, GateOutcome::UpdateInProgress) {
                    observed.set(true);
                }
                outcome
            },
            true,
            None,
            |_| {},
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        // The update finished, but this image saw it in progress: re-exec.
        assert!(needs_fresh_image(observed.get(), &outcome, true));
        // A launch that never saw an update, or a non-macOS one, proceeds.
        assert!(!needs_fresh_image(false, &outcome, true));
        assert!(!needs_fresh_image(true, &outcome, false));
        // Terminal stops are never turned into a restart.
        let recovery = GateOutcome::RecoveryRequired("x".into());
        assert!(!needs_fresh_image(true, &recovery, true));
        assert!(!needs_fresh_image(true, &GateOutcome::UpdateInProgress, true));
    }

    #[test]
    fn a_lease_wait_inside_the_gate_that_then_proceeds_still_needs_a_fresh_image() {
        // The update holds the startup lease: a non-waiting attempt times out,
        // while a waiting one outlasts the update and returns Unmanaged
        // directly, never reporting UpdateInProgress to the caller.
        let observed = std::cell::Cell::new(false);
        let mut waits = Vec::new();
        let outcome = gate_with_open_retry(
            |wait| {
                take_gate_observing_waits(
                    |attempt| {
                        waits.push(attempt);
                        if attempt.is_zero() {
                            GateOutcome::UpdateInProgress
                        } else {
                            GateOutcome::Unmanaged
                        }
                    },
                    wait,
                    &observed,
                )
            },
            true,
            None,
            |_| {},
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert!(observed.get(), "the lease wait must be observed");
        assert!(needs_fresh_image(observed.get(), &outcome, true));
        assert_eq!(waits.len(), 2);
    }

    #[test]
    fn a_helper_that_released_the_lease_before_the_probe_still_leaves_a_stale_image_detected() {
        // The probe sees no update and the gate proceeds directly, so nothing
        // was observed; the on-disk executable is a different generation.
        let observed = std::cell::Cell::new(false);
        let outcome = take_gate_observing_waits(
            |_| GateOutcome::Unmanaged,
            STARTUP_WAIT,
            &observed,
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert!(!observed.get());
        let mut new_image = vec![0u8; 3 << 20];
        new_image.extend_from_slice(b"hane-image-generation=99.0.0;");
        let disk_differs = !reader_contains(&new_image[..], image_marker()).unwrap();
        assert!(disk_differs);
        assert!(needs_fresh_image(observed.get() || disk_differs, &outcome, true));
    }

    #[test]
    fn an_unchanged_image_on_disk_is_not_restarted() {
        let mut same = vec![1u8; (1 << 20) - 5];
        same.extend_from_slice(image_marker());
        same.extend_from_slice(&[2u8; 100]);
        assert!(reader_contains(&same[..], image_marker()).unwrap());
        let outcome = GateOutcome::Unmanaged;
        assert!(!needs_fresh_image(false, &outcome, true));
    }

    #[test]
    fn an_unverified_disk_executable_is_never_reexecuted() {
        // No pinned Team ID: the verifier is not even consulted.
        let mut consulted = false;
        let result = reexec_trust_decision(None, |_| {
            consulted = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!consulted);
        // A failed signature/Gatekeeper check refuses and carries the reason.
        let err = reexec_trust_decision(Some("TEAM123456"), |_| Err("bad signature".into()))
            .unwrap_err();
        assert!(err.contains("bad signature"));
        // Only a verified bundle may be re-executed.
        assert!(reexec_trust_decision(Some("TEAM123456"), |team| {
            assert_eq!(team, "TEAM123456");
            Ok(())
        })
        .is_ok());
        // The refusal fails closed with a visible notice and no re-exec.
        let mut shown = 0;
        let entry = stop_after_failed_reexec(&err, |_| shown += 1);
        assert!(matches!(entry, Entry::Stop(EXIT_UPDATE_IN_PROGRESS)));
        assert_eq!(shown, 1);
    }

    #[test]
    fn an_unconfigured_production_policy_refuses_reexec() {
        let policy = TrustPolicy::new(Vec::new(), None);
        assert!(verify_exe_for_reexec(std::path::Path::new("/nonexistent/hane"), &policy).is_err());
    }

    #[test]
    fn a_failed_reexec_shows_a_user_visible_notice_and_stops() {
        let mut shown = Vec::new();
        let entry = stop_after_failed_reexec("No such file", |notice| {
            shown.push((notice.title.clone(), notice.message.clone()));
        });
        assert!(matches!(entry, Entry::Stop(EXIT_UPDATE_IN_PROGRESS)));
        assert_eq!(shown.len(), 1, "the notice is presented exactly once");
        let (title, message) = &shown[0];
        assert!(!title.is_empty());
        assert!(message.contains("No such file"), "the reason is shown");
        assert!(message.contains("もう一度Haneを起動"), "restart steps are given");
        assert!(message.contains("開く操作"), "the lost request is not hidden");
    }

    #[test]
    fn update_in_progress_refusal_is_shown_and_asks_to_retry_after_the_update() {
        let mut shown = Vec::new();
        let entry = stop_for_refusal(GateOutcome::UpdateInProgress, |n| {
            shown.push((n.title.clone(), n.message.clone()));
        });
        assert!(matches!(entry, Entry::Stop(EXIT_UPDATE_IN_PROGRESS)));
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].0, update_in_progress_notice().title);
        let message = &shown[0].1;
        assert!(message.contains("更新中のため"));
        assert!(message.contains("更新が終わってから"));
        assert!(message.contains("開く操作"));
    }

    #[test]
    fn recovery_refusal_is_shown_and_never_reads_as_a_successful_update() {
        let mut shown = Vec::new();
        let entry = stop_for_refusal(GateOutcome::RecoveryRequired("broken".into()), |n| {
            shown.push(n.message.clone());
        });
        assert!(matches!(entry, Entry::Stop(EXIT_RECOVERY_REQUIRED)));
        assert_eq!(shown.len(), 1);
        let message = &shown[0];
        assert!(message.contains("broken"), "the reason is shown");
        assert!(message.contains("復旧の案内に従"));
        assert!(message.contains("復旧が済んだ後で"));
        assert!(message.contains("開く操作"));
        assert!(!message.contains("更新は完了しています"));
        assert!(!message.contains("更新が終わってから"));
    }

    #[test]
    fn a_failed_child_confirmation_is_shown_not_left_on_stderr() {
        let mut shown = 0;
        let entry = stop_for_recovery("x", |_| shown += 1);
        assert!(matches!(entry, Entry::Stop(EXIT_RECOVERY_REQUIRED)));
        assert_eq!(shown, 1);
    }

    #[test]
    fn notices_use_the_native_surface_of_each_os() {
        assert_eq!(notice_surface(true, false), NoticeSurface::MacosAlert);
        assert_eq!(notice_surface(false, true), NoticeSurface::WindowsMessageBox);
        assert_eq!(notice_surface(false, false), NoticeSurface::StderrOnly);
    }

    #[test]
    fn windows_message_text_is_nul_terminated_utf16() {
        let wide = to_wide_nul("更新中A");
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(String::from_utf16(&wide[..wide.len() - 1]).unwrap(), "更新中A");
        assert_eq!(wide.iter().filter(|&&u| u == 0).count(), 1);
    }

    #[test]
    fn the_alert_command_passes_the_text_as_arguments_not_script_source() {
        let notice = reexec_failure_notice("bad \"quote\" \\ reason");
        let args = osascript_alert_args(&notice);
        let separator = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[separator + 1], notice.title);
        assert_eq!(args[separator + 2], notice.message);
        assert!(args[..separator].iter().all(|arg| !arg.contains("quote")));
    }

    #[test]
    fn an_uncontended_gate_is_not_treated_as_an_observed_update() {
        let observed = std::cell::Cell::new(false);
        let outcome = take_gate_observing_waits(
            |_| GateOutcome::Unmanaged,
            STARTUP_WAIT,
            &observed,
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert!(!observed.get());
        assert!(!needs_fresh_image(observed.get(), &outcome, true));
    }

    #[test]
    fn a_pending_finder_open_is_handed_over_once_across_the_restart() {
        // The event stays with the OS (same PID after exec) and is queued only
        // by the fresh image after admission, so it opens exactly once.
        let queue = hane_ui::SharedExternalOpenQueue::new();
        let mut opened = Vec::new();
        queue.push(PathBuf::from("/notes/a.md"));
        for _ in 0..2 {
            queue.drain_into(|path| {
                opened.push(path.to_path_buf());
                hane_ui::ExternalOpenOutcome::Accepted
            });
        }
        assert_eq!(opened, vec![PathBuf::from("/notes/a.md")]);
    }

    #[test]
    fn a_launch_that_may_receive_a_finder_open_waits_for_the_update() {
        assert!(may_carry_open_request(true));
        assert_eq!(may_carry_open_request(false), cfg!(target_os = "macos"));
    }

    #[test]
    fn a_finder_open_before_update_completion_is_opened_once_after_admission() {
        let queue = hane_ui::SharedExternalOpenQueue::new();
        // The event is queued while the update is still in progress.
        queue.push(PathBuf::from("/notes/a.md"));
        let mut opened = Vec::new();
        let mut inner = scripted(2);
        let outcome = gate_with_open_retry(
            |wait| {
                let outcome = inner(wait);
                if matches!(outcome, GateOutcome::UpdateInProgress) {
                    // Nothing opens before the gate admits normal editing.
                    assert!(opened.is_empty());
                }
                outcome
            },
            true,
            Some(Duration::from_secs(60)),
            |_| {},
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        for _ in 0..2 {
            queue.drain_into(|path| {
                opened.push(path.to_path_buf());
                hane_ui::ExternalOpenOutcome::Accepted
            });
        }
        assert_eq!(opened, vec![PathBuf::from("/notes/a.md")]);
        assert!(queue.is_empty());
    }

    #[test]
    fn a_launch_without_an_explicit_open_still_stops_while_updating() {
        let mut sleeps = 0;
        let outcome = gate_with_open_retry(
            scripted(3),
            false,
            Some(Duration::from_secs(60)),
            |_| sleeps += 1,
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert_eq!(sleeps, 0);
    }

    #[test]
    fn an_explicit_open_gives_up_after_the_wait_without_bypassing_the_gate() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut sleeps = 0;
        let outcome = gate_with_open_retry(
            |_| GateOutcome::UpdateInProgress,
            true,
            Some(Duration::from_secs(1)),
            |d| {
                sleeps += 1;
                clock.set(clock.get() + d);
            },
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert_eq!(sleeps, 4);
    }

    #[test]
    fn a_launch_without_a_time_limit_outlives_the_old_budget_and_is_admitted_once() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let queue = hane_ui::SharedExternalOpenQueue::new();
        let mut admitted = 0;
        let mut opened = Vec::new();
        let outcome = gate_with_open_retry(
            |wait| {
                clock.set(clock.get() + wait);
                // The update outlasts both the 60s budget and many attempts.
                if clock.get() <= Duration::from_secs(600) {
                    assert!(opened.is_empty(), "nothing opens before admission");
                    GateOutcome::UpdateInProgress
                } else {
                    admitted += 1;
                    GateOutcome::Unmanaged
                }
            },
            true,
            None,
            |d| clock.set(clock.get() + d),
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert_eq!(admitted, 1);
        assert!(clock.get() > Duration::from_secs(600));
        // The Finder event arrives only after admission and opens exactly once.
        queue.push(PathBuf::from("/notes/a.md"));
        for _ in 0..2 {
            queue.drain_into(|path| {
                opened.push(path.to_path_buf());
                hane_ui::ExternalOpenOutcome::Accepted
            });
        }
        assert_eq!(opened, vec![PathBuf::from("/notes/a.md")]);
    }

    #[test]
    fn a_launch_without_a_time_limit_still_stops_on_recovery_required() {
        let mut calls = 0;
        let outcome = gate_with_open_retry(
            |_| {
                calls += 1;
                if calls < 3 {
                    GateOutcome::UpdateInProgress
                } else {
                    GateOutcome::RecoveryRequired("broken".into())
                }
            },
            true,
            None,
            |_| {},
            || Duration::ZERO,
        );
        assert!(matches!(outcome, GateOutcome::RecoveryRequired(_)));
        assert_eq!(calls, 3);
    }

    #[test]
    fn only_macos_and_windows_cli_launches_are_unbounded() {
        assert_eq!(open_request_wait(false), None);
        if cfg!(any(target_os = "macos", target_os = "windows")) {
            assert_eq!(open_request_wait(true), None);
        } else {
            assert_eq!(open_request_wait(true), Some(EXPLICIT_OPEN_WAIT));
        }
        // Linux CLI stays bounded; macOS and Windows CLI are unbounded.
        assert_eq!(
            open_request_wait_for(true, false, false),
            Some(EXPLICIT_OPEN_WAIT)
        );
        assert_eq!(open_request_wait_for(true, true, false), None);
        assert_eq!(open_request_wait_for(true, false, true), None);
        assert_eq!(open_request_wait_for(false, true, false), None);
    }

    #[test]
    fn a_windows_cli_open_outlives_the_old_60s_limit_and_is_handed_over_once() {
        let wait = open_request_wait_for(true, false, true);
        assert_eq!(wait, None);
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut admitted = 0;
        let mut opened = Vec::new();
        let outcome = gate_with_open_retry(
            |attempt| {
                clock.set(clock.get() + attempt);
                if clock.get() <= Duration::from_secs(120) {
                    GateOutcome::UpdateInProgress
                } else {
                    admitted += 1;
                    GateOutcome::Unmanaged
                }
            },
            true,
            wait,
            |d| clock.set(clock.get() + d),
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::Unmanaged));
        assert_eq!(admitted, 1);
        assert!(clock.get() > Duration::from_secs(60));
        // Only after admission is the path passed to the normal open flow, once.
        if matches!(outcome, GateOutcome::Unmanaged) {
            opened.push(PathBuf::from("C:\\notes\\a.md"));
        }
        assert_eq!(opened, vec![PathBuf::from("C:\\notes\\a.md")]);
    }

    #[test]
    fn context_menu_flags_are_not_open_paths_but_ordinary_paths_are() {
        use std::ffi::OsStr;
        assert!(!is_cli_open_path(None));
        assert!(!is_cli_open_path(Some(OsStr::new("--register-context-menu"))));
        assert!(!is_cli_open_path(Some(OsStr::new("--unregister-context-menu"))));
        assert!(is_cli_open_path(Some(OsStr::new("C:\\notes\\a.md"))));
        assert!(is_cli_open_path(Some(OsStr::new("a.md"))));
        // A management flag keeps the bounded gate on Windows; a path waits
        // for the terminal outcome.
        let flag = is_cli_open_path(Some(OsStr::new("--register-context-menu")));
        let gate = gate_with_open_retry(
            |_| GateOutcome::UpdateInProgress,
            flag,
            open_request_wait_for(flag, false, true),
            |_| panic!("a management flag never retries"),
            || Duration::ZERO,
        );
        assert!(matches!(gate, GateOutcome::UpdateInProgress));
        assert_eq!(open_request_wait_for(true, false, true), None);
    }

    #[test]
    fn a_linux_cli_open_stays_bounded() {
        let wait = open_request_wait_for(true, false, false);
        assert_eq!(wait, Some(EXPLICIT_OPEN_WAIT));
        let clock = std::cell::Cell::new(Duration::ZERO);
        let outcome = gate_with_open_retry(
            |attempt| {
                clock.set(clock.get() + attempt);
                GateOutcome::UpdateInProgress
            },
            true,
            wait,
            |d| clock.set(clock.get() + d),
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert!(clock.get() <= Duration::from_secs(60));
    }

    #[test]
    fn time_spent_inside_gate_calls_counts_toward_the_total_wait() {
        let clock = std::cell::Cell::new(Duration::ZERO);
        let mut calls = 0;
        let mut granted = Vec::new();
        let outcome = gate_with_open_retry(
            |wait| {
                calls += 1;
                granted.push(wait);
                // Each attempt consumes its whole grant, like a lease timeout.
                clock.set(clock.get() + wait);
                GateOutcome::UpdateInProgress
            },
            true,
            Some(Duration::from_secs(60)),
            |d| clock.set(clock.get() + d),
            || clock.get(),
        );
        assert!(matches!(outcome, GateOutcome::UpdateInProgress));
        assert!(
            clock.get() <= Duration::from_secs(60),
            "total wait {:?} exceeded the budget",
            clock.get()
        );
        assert!(granted.iter().all(|w| *w <= STARTUP_WAIT));
        assert!(calls <= 3, "{calls} gate calls cannot fit a 60s budget of 30s waits");
    }
}
