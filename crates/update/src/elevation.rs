//! Decisions of the on-demand Windows UAC elevation of the update helper
//! (Issue #510). Everything here is plain safe Rust behind injectable OS
//! seams, so the policy is tested without a real UAC prompt:
//!
//! - [`choose_route`]: whether the helper is started normally or through a
//!   consent prompt. Only a known protected placement lacking write access, or
//!   a verified helper whose normal start failed with `ERROR_ELEVATION_REQUIRED`
//!   (740), escalates. A plain access-denied never escalates by itself.
//! - [`check_principals`]: the standard user, the owner of the install and the
//!   elevated token are different facts. An unverifiable relation is refused
//!   before any change; the `InstallId` is never derived again for the elevated
//!   principal.
//! - [`verify_peer`] / [`NonceLedger`]: the elevated helper is accepted only
//!   when the OS-reported process facts and a one-shot secret both match.
//! - [`launch_elevated`]: one consent request through an [`ElevatedLauncher`];
//!   the user's refusal is a cancellation that leaves the running application
//!   and its work untouched and can be retried.
//! - [`connect_elevated`]: the whole application side up to a verified peer:
//!   principals, one consent request, and the local channel of the process that
//!   was really launched. Only a peer whose OS-reported process id, session,
//!   token account and elevation all match is handed back; anything else stops
//!   the launched process and reports whether it is certainly gone.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use crate::error::{ErrorCode, Result, UpdateError};
use crate::identity::TransactionId;
use crate::protocol::Nonce;

/// The single fixed argument of an elevated helper start. The helper takes no
/// other argument, path or secret on its command line.
pub const ELEVATED_ARGUMENT: &str = "--elevated";

/// Whether this build can restart the new version with the ordinary (not the
/// elevated) token after an elevated apply. The elevated helper never lets the
/// limited-health child inherit its administrator token: on Windows it creates
/// the child suspended with the requesting user's unelevated shell token, checks
/// the OS-reported token, session and image of the created process
/// ([`verify_standard_child`]) and binds the capability to its process id and
/// creation time. Where that cannot be done the route stays closed before any
/// consent prompt (see [`ElevationOs::standard_restart_available`]), and the
/// helper refuses to apply.
pub const STANDARD_RESTART_AVAILABLE: bool = cfg!(windows);

/// `CreateProcess` result when the image needs elevation.
pub const ERROR_ELEVATION_REQUIRED: u32 = 740;
/// The user declined the consent prompt or cancelled the credential entry.
pub const ERROR_CANCELLED: u32 = 1223;

/// The message shown when the user declines the Windows consent prompt.
pub const CANCELLED_MESSAGE: &str = "Windowsの確認画面で更新が許可されなかったため、更新を中止しました。現在のHaneと作業内容はそのままです。あとで「再起動して更新」をやり直せます。";
/// The notice shown before "再起動して更新" may open the Windows prompt.
pub const CONSENT_NOTICE: &str = "この配置では更新の適用にWindowsの確認画面が表示されることがあります。";

/// Marker in [`UpdateError::detail`] that tells a declined prompt from other
/// cancellations.
pub const DECLINED_MARKER: &str = "elevation_declined";

// ------------------------------------------------------------------ routing

/// How the OS classified a failed helper start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchClass {
    /// The image itself requires elevation (740).
    ElevationRequired,
    /// The user declined the consent prompt (1223).
    UserCancelled,
    /// Anything else, access denied included: never a reason to escalate.
    Other,
}

pub fn classify_launch_error(os_error: Option<u32>) -> LaunchClass {
    match os_error {
        Some(ERROR_ELEVATION_REQUIRED) => LaunchClass::ElevationRequired,
        Some(ERROR_CANCELLED) => LaunchClass::UserCancelled,
        _ => LaunchClass::Other,
    }
}

/// Whether the install location can be written by the current token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAccess {
    Writable,
    Denied,
    Unknown,
}

/// The result of starting the verified helper without elevation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardAttempt {
    NotTried,
    Started,
    Failed { os_error: Option<u32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationReason {
    /// A recognised protected placement without write access.
    ProtectedPlacement,
    /// The verified helper's normal start returned 740.
    LaunchRequiresElevation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Writable and startable: no prompt.
    Standard,
    Elevated(ElevationReason),
    Refused(String),
}

/// `protected_placement` is true only for a placement whose owner and ACL were
/// recognised as a protected one; an unknown directory is never assumed to be.
pub fn choose_route(
    protected_placement: bool,
    write: WriteAccess,
    attempt: StandardAttempt,
) -> Route {
    match attempt {
        StandardAttempt::Failed { os_error } => match classify_launch_error(os_error) {
            LaunchClass::ElevationRequired => Route::Elevated(ElevationReason::LaunchRequiresElevation),
            LaunchClass::UserCancelled => Route::Refused("the helper start was cancelled".to_owned()),
            LaunchClass::Other => Route::Refused(format!(
                "the helper could not be started (os_error={})",
                os_error.map_or_else(|| "none".to_owned(), |e| e.to_string())
            )),
        },
        StandardAttempt::Started => Route::Standard,
        StandardAttempt::NotTried => match (write, protected_placement) {
            (WriteAccess::Writable, _) => Route::Standard,
            (WriteAccess::Denied, true) => Route::Elevated(ElevationReason::ProtectedPlacement),
            (WriteAccess::Denied, false) => Route::Refused(
                "the install location is not writable and is not a recognised protected placement"
                    .to_owned(),
            ),
            (WriteAccess::Unknown, _) => {
                Route::Refused("write access to the install location could not be determined".to_owned())
            }
        },
    }
}

// --------------------------------------------------------------- principals

const SID_ADMINISTRATORS: &str = "S-1-5-32-544";
const SID_SYSTEM: &str = "S-1-5-18";
const SID_TRUSTED_INSTALLER_PREFIX: &str = "S-1-5-80-";

/// The three distinct principals of an elevated update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principals {
    /// The (standard) user who runs the application and owns its state.
    pub user_sid: String,
    /// The owner of the install root as the OS reports it.
    pub install_owner_sid: String,
    /// The token the elevated helper runs as (possibly another administrator).
    pub elevated_sid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalDecision {
    /// The elevated token is another account than the requesting user.
    pub other_administrator: bool,
    /// Always false: `InstallId` and state come from the requesting side.
    pub rederive_install_id: bool,
}

// The OS adapters report a SID as `sid:` and the hex of its binary form.
const BINARY_SID_ADMINISTRATORS: &str = "sid:01020000000000052000000020020000";
const BINARY_SID_SYSTEM: &str = "sid:010100000000000512000000";
const BINARY_SID_TRUSTED_INSTALLER_PREFIX: &str = "sid:010600000000000550000000";

fn is_protected_owner(sid: &str) -> bool {
    sid == SID_ADMINISTRATORS
        || sid == SID_SYSTEM
        || sid.starts_with(SID_TRUSTED_INSTALLER_PREFIX)
        || sid == BINARY_SID_ADMINISTRATORS
        || sid == BINARY_SID_SYSTEM
        || sid.starts_with(BINARY_SID_TRUSTED_INSTALLER_PREFIX)
}

/// The OS facts of an install placement that decide the route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacementAccess {
    /// The owner is the user or a protected owner and the directory lies under
    /// a system-protected root (for example `Program Files`).
    pub protected: bool,
    /// Whether the current (ordinary) token verifiably writes the install.
    pub write: WriteAccess,
}

/// Whether `install_root` is a known protected placement. An owner that is
/// neither the requesting user nor a protected owner, or a directory outside
/// every protected root, is never assumed to be one.
pub fn is_protected_placement(
    user_sid: &str,
    install_owner_sid: &str,
    install_root: &Path,
    protected_roots: &[std::path::PathBuf],
) -> bool {
    let owner_known = !user_sid.is_empty()
        && !install_owner_sid.is_empty()
        && (install_owner_sid == user_sid || is_protected_owner(install_owner_sid));
    let normalize = |path: &Path| {
        let text = path.display().to_string();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text).replace('/', "\\");
        text.trim_end_matches('\\').to_ascii_lowercase()
    };
    let root = normalize(install_root);
    owner_known
        && protected_roots.iter().any(|protected| {
            let protected = normalize(protected);
            !protected.is_empty()
                && root.len() > protected.len()
                && root.starts_with(&protected)
                && root.as_bytes()[protected.len()] == b'\\'
        })
}

/// The refusal of the update before any change when the route cannot be chosen.
pub fn route_refused_error(reason: &str) -> UpdateError {
    UpdateError::new(ErrorCode::Io, format!("the update route was refused: {reason}"))
}

/// A known protected placement that needs the consent prompt, in a build that
/// cannot finish an elevated apply.
pub fn protected_placement_error() -> UpdateError {
    UpdateError::new(
        ErrorCode::Io,
        format!("{ELEVATION_REQUIRED_MARKER}: the install is a protected placement without write access"),
    )
}

/// Accepts the relation only when the install owner is the requesting user or a
/// protected owner, and every SID is present. Anything else is refused as a
/// whole, never recorded as handled.
pub fn check_principals(principals: &Principals) -> Result<PrincipalDecision> {
    let refuse = |why: &str| Err(UpdateError::new(ErrorCode::IdentityMismatch, why.to_owned()));
    if principals.user_sid.is_empty()
        || principals.install_owner_sid.is_empty()
        || principals.elevated_sid.is_empty()
    {
        return refuse("a principal of the elevated update could not be determined");
    }
    let owner_known = principals.install_owner_sid == principals.user_sid
        || is_protected_owner(&principals.install_owner_sid);
    if !owner_known {
        return refuse("the install is owned by an account that cannot be verified for elevation");
    }
    Ok(PrincipalDecision {
        other_administrator: principals.elevated_sid != principals.user_sid,
        rederive_install_id: false,
    })
}

// --------------------------------------------------------------------- peer

/// What the OS says about the connected process. Never self-reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerFacts {
    pub pid: u32,
    pub session_id: u32,
    pub token_sid: String,
    pub token_elevated: bool,
}

/// What the application expects, from the process handle it got at launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedPeer {
    pub pid: u32,
    pub session_id: u32,
    pub token_sid: String,
}

pub fn verify_peer(os: &PeerFacts, expected: &ExpectedPeer) -> Result<()> {
    let refuse = |why: &str| Err(UpdateError::new(ErrorCode::IdentityMismatch, why.to_owned()));
    if os.pid != expected.pid {
        return refuse("the connected process is not the launched helper");
    }
    if os.session_id != expected.session_id {
        return refuse("the connected process runs in another session");
    }
    if os.token_sid != expected.token_sid {
        return refuse("the connected process runs as another account");
    }
    if !os.token_elevated {
        return refuse("the connected process is not elevated");
    }
    Ok(())
}

/// Accepts a token as the requesting user's ordinary token: it is the expected
/// account and it is not elevated.
pub fn require_standard_token(token_sid: &str, token_elevated: bool, expected_sid: &str) -> Result<()> {
    let refuse = |why: &str| Err(UpdateError::new(ErrorCode::IdentityMismatch, why.to_owned()));
    if expected_sid.is_empty() || token_sid != expected_sid {
        return refuse("the token is not the requesting user's account");
    }
    if token_elevated {
        return refuse("the token is elevated");
    }
    Ok(())
}

/// What the OS says about the process the elevated helper created for the new
/// version. Never self-reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardChildFacts {
    pub pid: u32,
    pub session_id: u32,
    pub token_sid: String,
    pub token_elevated: bool,
    pub image_path: String,
}

/// What the helper expects of that process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedStandardChild {
    pub pid: u32,
    pub session_id: u32,
    pub token_sid: String,
    pub image_path: String,
}

fn same_image(a: &str, b: &str) -> bool {
    let plain = |path: &'_ str| path.strip_prefix(r"\\?\").unwrap_or(path).replace('/', "\\");
    !a.is_empty() && plain(a).eq_ignore_ascii_case(&plain(b))
}

/// The created new version is proven to be the process that was asked for, in
/// the requesting session, running as the requesting account with an
/// unelevated token, from the verified executable. An administrator token that
/// leaked into the child, another account, session, process or image is
/// refused.
pub fn verify_standard_child(os: &StandardChildFacts, expected: &ExpectedStandardChild) -> Result<()> {
    let refuse = |why: &str| Err(UpdateError::new(ErrorCode::IdentityMismatch, why.to_owned()));
    if os.pid != expected.pid {
        return refuse("the created process is not the requested one");
    }
    if os.session_id != expected.session_id {
        return refuse("the created process runs in another session");
    }
    require_standard_token(&os.token_sid, os.token_elevated, &expected.token_sid)?;
    if !same_image(&os.image_path, &expected.image_path) {
        return refuse("the created process runs another executable");
    }
    Ok(())
}

/// One-shot secrets of the elevated sessions: a nonce is valid for exactly one
/// transaction and one use.
#[derive(Default)]
pub struct NonceLedger {
    issued: Vec<(String, String)>,
    used: BTreeSet<String>,
}

impl NonceLedger {
    pub fn issue(&mut self, transaction: &str, nonce: &Nonce) {
        self.issued.push((transaction.to_owned(), nonce.as_str().to_owned()));
    }

    /// Consumes the nonce. A second use, another transaction or an unknown
    /// value is refused.
    pub fn consume(&mut self, transaction: &str, presented: &str) -> Result<()> {
        let refuse = |why: &str| Err(UpdateError::new(ErrorCode::IdentityMismatch, why.to_owned()));
        if self.used.contains(presented) {
            return refuse("the session secret was already used");
        }
        match self.issued.iter().find(|(_, nonce)| nonce == presented) {
            Some((issued_for, _)) if issued_for == transaction => {
                self.used.insert(presented.to_owned());
                Ok(())
            }
            Some(_) => refuse("the session secret belongs to another transaction"),
            None => refuse("the session secret is unknown"),
        }
    }
}

// ------------------------------------------------------------------- launch

/// A failed consent launch, with the facts the OS gave (no secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchFailure {
    pub stage: &'static str,
    pub api: &'static str,
    pub os_error: Option<u32>,
    pub path: String,
}

/// A launched elevated process whose real handle the application keeps.
pub trait ElevatedProcess: Send {
    fn pid(&self) -> u32;
    /// `Some(code)` once the process has exited within `timeout`.
    fn wait_exit(&mut self, timeout: Duration) -> Option<u32>;
    /// Best-effort stop of a helper that never became ready. The caller
    /// confirms the exit with [`ElevatedProcess::wait_exit`]; an exit that
    /// cannot be confirmed keeps the transaction for a later cleanup.
    fn terminate(&mut self);
}

/// The OS seam of `ShellExecuteExW` with the `runas` verb.
pub trait ElevatedLauncher {
    type Process: ElevatedProcess;
    fn launch(&mut self, helper: &std::path::Path) -> std::result::Result<Self::Process, LaunchFailure>;
}

/// Asks for consent once and starts `helper`. A declined prompt is
/// [`ErrorCode::Cancelled`] carrying [`DECLINED_MARKER`]; every other failure
/// keeps its stage, API, OS error number and path.
pub fn launch_elevated<L: ElevatedLauncher>(
    launcher: &mut L,
    helper: &std::path::Path,
) -> Result<L::Process> {
    launcher.launch(helper).map_err(launch_failure_error)
}

/// The error of a failed consent launch (see [`launch_elevated`]).
pub fn launch_failure_error(failure: LaunchFailure) -> UpdateError {
    if classify_launch_error(failure.os_error) == LaunchClass::UserCancelled {
        UpdateError::new(ErrorCode::Cancelled, format!("{DECLINED_MARKER}: {CANCELLED_MESSAGE}"))
    } else {
        UpdateError::new(
            ErrorCode::Io,
            format!(
                "the elevated helper could not be started (stage={}, api={}, os_error={}, path={})",
                failure.stage,
                failure.api,
                failure
                    .os_error
                    .map_or_else(|| "none".to_owned(), |e| e.to_string()),
                failure.path
            ),
        )
    }
}

// --------------------------------------------------------------- connection

/// A connection accepted by the application's local endpoint, with the facts
/// the OS reports about the process on the other end.
pub struct AcceptedPeer {
    pub facts: PeerFacts,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

/// The application's local endpoint for one elevated helper. It exists before
/// the helper is launched and accepts exactly one connection.
pub trait ElevatedListener: Send {
    /// Waits for the helper. `helper_alive` is asked while waiting so that a
    /// helper that already ended is not waited for until the timeout.
    fn accept(
        &mut self,
        timeout: Duration,
        helper_alive: &mut dyn FnMut() -> bool,
    ) -> Result<AcceptedPeer>;
}

/// The operating system under the elevated route, as one injectable seam.
pub trait ElevationOs {
    /// See [`STANDARD_RESTART_AVAILABLE`].
    fn standard_restart_available(&self) -> bool;
    /// The classification and the verified write access of the install, which
    /// alone decide whether the helper needs the consent prompt.
    fn placement_access(&self, install_root: &Path) -> PlacementAccess;
    /// `(requesting user's SID, owner SID of the install root)` as the OS
    /// reports them.
    fn current_principals(&self, install_root: &Path) -> Result<(String, String)>;
    /// The session of this (the requesting) process.
    fn session_id(&self) -> Result<u32>;
    /// Creates the local endpoint of `transaction` before any launch.
    fn listen(&self, transaction: &TransactionId) -> Result<Box<dyn ElevatedListener>>;
    /// One consent request that starts `helper` elevated.
    fn launch(&self, helper: &Path) -> std::result::Result<Box<dyn ElevatedProcess>, LaunchFailure>;
}

/// The seam of an OS without the elevated route (everything but Windows).
pub struct UnavailableElevation;

impl ElevationOs for UnavailableElevation {
    fn standard_restart_available(&self) -> bool {
        false
    }

    /// Without an elevated route nothing is ever prompted for: the helper is
    /// started normally and a placement it cannot write fails at the apply.
    fn placement_access(&self, _install_root: &Path) -> PlacementAccess {
        PlacementAccess { protected: false, write: WriteAccess::Writable }
    }

    fn current_principals(&self, _install_root: &Path) -> Result<(String, String)> {
        Err(UpdateError::new(ErrorCode::NotApplicable, "this OS has no elevated update route"))
    }

    fn session_id(&self) -> Result<u32> {
        Err(UpdateError::new(ErrorCode::NotApplicable, "this OS has no elevated update route"))
    }

    fn listen(&self, _transaction: &TransactionId) -> Result<Box<dyn ElevatedListener>> {
        Err(UpdateError::new(ErrorCode::NotApplicable, "this OS has no elevated update route"))
    }

    fn launch(&self, helper: &Path) -> std::result::Result<Box<dyn ElevatedProcess>, LaunchFailure> {
        Err(LaunchFailure {
            stage: "unavailable",
            api: "none",
            os_error: None,
            path: helper.display().to_string(),
        })
    }
}

/// A verified elevated helper and the application's ends of its channel.
pub struct ElevatedConnection {
    pub process: Box<dyn ElevatedProcess>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

/// Why the elevated helper could not be connected, and whether it is certain
/// not to be running (it never started, or its exit was confirmed).
pub struct ConnectFailure {
    pub error: UpdateError,
    pub helper_stopped: bool,
}

/// How long the application waits for the elevated helper to connect once the
/// consent was given.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a stopped helper gets to be confirmed gone.
pub const STOP_WAIT: Duration = Duration::from_secs(2);

/// Stops `process` (best effort) and reports `error` together with whether its
/// exit was confirmed.
pub fn stop_helper(mut process: Box<dyn ElevatedProcess>, error: UpdateError) -> ConnectFailure {
    process.terminate();
    let helper_stopped = process.wait_exit(STOP_WAIT).is_some();
    ConnectFailure { error, helper_stopped }
}

/// Starts `helper` through one consent request and returns its channel once the
/// connected process is proven to be that very process, elevated, in this
/// session and running as the requesting account. Nothing is prompted for when
/// the route is unavailable or the principals cannot be verified, and a refused
/// prompt changes nothing.
pub fn connect_elevated(
    os: &dyn ElevationOs,
    install_root: &Path,
    helper: &Path,
    transaction: &TransactionId,
    timeout: Duration,
) -> std::result::Result<ElevatedConnection, ConnectFailure> {
    let unstarted = |error: UpdateError| ConnectFailure { error, helper_stopped: true };
    if !os.standard_restart_available() {
        return Err(unstarted(UpdateError::new(
            ErrorCode::Io,
            format!("{ELEVATION_REQUIRED_MARKER}: the elevated apply is not available in this build"),
        )));
    }
    let (user_sid, install_owner_sid) = os.current_principals(install_root).map_err(unstarted)?;
    // The elevated token must be the requesting account: an unverifiable
    // owner relation is refused here, and another account is refused when the
    // peer is verified below.
    check_principals(&Principals {
        user_sid: user_sid.clone(),
        install_owner_sid,
        elevated_sid: user_sid.clone(),
    })
    .map_err(unstarted)?;
    let session_id = os.session_id().map_err(unstarted)?;
    let mut listener = os.listen(transaction).map_err(unstarted)?;
    let mut process = os
        .launch(helper)
        .map_err(|failure| unstarted(launch_failure_error(failure)))?;
    let expected = ExpectedPeer { pid: process.pid(), session_id, token_sid: user_sid };
    let accepted = listener.accept(timeout, &mut || process.wait_exit(Duration::ZERO).is_none());
    match accepted.and_then(|peer| verify_peer(&peer.facts, &expected).map(|()| peer)) {
        Ok(peer) => Ok(ElevatedConnection { process, reader: peer.reader, writer: peer.writer }),
        Err(error) => Err(stop_helper(process, error)),
    }
}

/// Marker in [`UpdateError::detail`] of a normal helper start that the OS
/// refused with `ERROR_ELEVATION_REQUIRED` (740).
pub const ELEVATION_REQUIRED_MARKER: &str = "elevation_required";

/// Shown when the helper needs the Windows consent prompt but the elevated
/// apply is not available in this build. Nothing was changed.
pub const ELEVATION_UNAVAILABLE_MESSAGE: &str = "この配置では更新の適用にWindowsの管理者確認が必要ですが、このバージョンでは対応していません。現在のHaneと作業内容はそのままです。書き込み可能な場所へ配置するか、手動で更新してください。";

/// The failure of starting the verified helper normally. No OS error number,
/// 740 included, is evidence that the install needs elevation (it can come from
/// the helper's file name alone): the number, stage, API and path are kept for
/// the record (no secrets) and the route is never changed by them.
pub fn normal_start_error(os_error: Option<u32>, path: &std::path::Path) -> UpdateError {
    let facts = format!(
        "stage=normal-launch, api=CreateProcessW, os_error={}, path={}",
        os_error.map_or_else(|| "none".to_owned(), |e| e.to_string()),
        path.display()
    );
    UpdateError::new(ErrorCode::Io, format!("the helper could not be started ({facts})"))
}

/// Whether `err` is a protected placement that needs elevation.
pub fn is_elevation_required(err: &UpdateError) -> bool {
    err.code == ErrorCode::Io && err.detail.starts_with(ELEVATION_REQUIRED_MARKER)
}

/// The Japanese text for the elevation related failures, if `err` is one.
pub fn elevation_message(err: &UpdateError) -> Option<&'static str> {
    if is_declined(err) {
        Some(CANCELLED_MESSAGE)
    } else if is_elevation_required(err) {
        Some(ELEVATION_UNAVAILABLE_MESSAGE)
    } else {
        None
    }
}

/// Whether `err` is the user's refusal of the consent prompt.
pub fn is_declined(err: &UpdateError) -> bool {
    err.code == ErrorCode::Cancelled && err.detail.starts_with(DECLINED_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_writable_placement_never_prompts() {
        assert_eq!(
            choose_route(false, WriteAccess::Writable, StandardAttempt::NotTried),
            Route::Standard
        );
        assert_eq!(
            choose_route(true, WriteAccess::Writable, StandardAttempt::Started),
            Route::Standard
        );
    }

    #[test]
    fn a_protected_placement_without_write_access_escalates() {
        assert_eq!(
            choose_route(true, WriteAccess::Denied, StandardAttempt::NotTried),
            Route::Elevated(ElevationReason::ProtectedPlacement)
        );
    }

    #[test]
    fn access_denied_alone_does_not_escalate() {
        assert!(matches!(
            choose_route(false, WriteAccess::Denied, StandardAttempt::NotTried),
            Route::Refused(_)
        ));
        assert!(matches!(
            choose_route(true, WriteAccess::Unknown, StandardAttempt::NotTried),
            Route::Refused(_)
        ));
        // ERROR_ACCESS_DENIED (5) on the helper start is not 740.
        assert!(matches!(
            choose_route(true, WriteAccess::Denied, StandardAttempt::Failed { os_error: Some(5) }),
            Route::Refused(_)
        ));
    }

    #[test]
    fn only_740_on_the_normal_start_escalates_and_is_not_a_protected_proof() {
        assert_eq!(
            choose_route(false, WriteAccess::Writable, StandardAttempt::Failed { os_error: Some(740) }),
            Route::Elevated(ElevationReason::LaunchRequiresElevation)
        );
        assert!(matches!(
            choose_route(false, WriteAccess::Writable, StandardAttempt::Failed { os_error: None }),
            Route::Refused(_)
        ));
        assert!(matches!(
            choose_route(false, WriteAccess::Writable, StandardAttempt::Failed { os_error: Some(1223) }),
            Route::Refused(_)
        ));
    }

    #[test]
    fn only_a_known_owner_under_a_protected_root_is_a_protected_placement() {
        let roots = [std::path::PathBuf::from(r"C:\Program Files")];
        let under = Path::new(r"C:\Program Files\Hane");
        let user = "sid:0102";
        assert!(is_protected_placement(user, user, under, &roots));
        assert!(is_protected_placement(user, BINARY_SID_ADMINISTRATORS, under, &roots));
        assert!(is_protected_placement(user, SID_SYSTEM, Path::new(r"\\?\c:\program files\Hane"), &roots));
        // Another owner, an unknown directory, the root itself and a sibling
        // that only shares the prefix are never assumed to be protected.
        assert!(!is_protected_placement(user, "sid:0999", under, &roots));
        assert!(!is_protected_placement(user, "", under, &roots));
        assert!(!is_protected_placement(user, user, Path::new(r"C:\Users\me\Hane"), &roots));
        assert!(!is_protected_placement(user, user, Path::new(r"C:\Program Files"), &roots));
        assert!(!is_protected_placement(user, user, Path::new(r"C:\Program Files Extra\Hane"), &roots));
        assert!(!is_protected_placement(user, user, under, &[]));
    }

    fn principals(user: &str, owner: &str, elevated: &str) -> Principals {
        Principals {
            user_sid: user.to_owned(),
            install_owner_sid: owner.to_owned(),
            elevated_sid: elevated.to_owned(),
        }
    }

    #[test]
    fn the_same_user_and_a_protected_owner_are_accepted() {
        let same = check_principals(&principals("S-1-5-21-1", "S-1-5-21-1", "S-1-5-21-1")).unwrap();
        assert!(!same.other_administrator && !same.rederive_install_id);
        let protected = check_principals(&principals("S-1-5-21-1", SID_ADMINISTRATORS, "S-1-5-21-1")).unwrap();
        assert!(!protected.other_administrator);
    }

    #[test]
    fn another_administrator_never_rederives_the_install_id() {
        let decision = check_principals(&principals("S-1-5-21-1", "S-1-5-21-1", "S-1-5-21-2")).unwrap();
        assert!(decision.other_administrator);
        assert!(!decision.rederive_install_id);
    }

    #[test]
    fn an_unverifiable_ownership_is_refused() {
        for p in [
            principals("S-1-5-21-1", "S-1-5-21-9", "S-1-5-21-1"),
            principals("", "S-1-5-21-1", "S-1-5-21-1"),
            principals("S-1-5-21-1", "", "S-1-5-21-1"),
            principals("S-1-5-21-1", "S-1-5-21-1", ""),
        ] {
            assert_eq!(check_principals(&p).unwrap_err().code, ErrorCode::IdentityMismatch);
        }
    }

    fn facts() -> PeerFacts {
        PeerFacts { pid: 10, session_id: 1, token_sid: "S-1-5-21-2".to_owned(), token_elevated: true }
    }

    fn expected() -> ExpectedPeer {
        ExpectedPeer { pid: 10, session_id: 1, token_sid: "S-1-5-21-2".to_owned() }
    }

    #[test]
    fn the_genuine_peer_is_accepted_and_every_forgery_refused() {
        verify_peer(&facts(), &expected()).unwrap();
        let mut wrong_pid = facts();
        wrong_pid.pid = 11;
        let mut wrong_session = facts();
        wrong_session.session_id = 2;
        let mut wrong_sid = facts();
        wrong_sid.token_sid = "S-1-5-21-3".to_owned();
        let mut not_elevated = facts();
        not_elevated.token_elevated = false;
        for forged in [wrong_pid, wrong_session, wrong_sid, not_elevated] {
            assert_eq!(verify_peer(&forged, &expected()).unwrap_err().code, ErrorCode::IdentityMismatch);
        }
    }

    fn child_facts() -> StandardChildFacts {
        StandardChildFacts {
            pid: 500,
            session_id: 1,
            token_sid: "S-1-5-21-1".to_owned(),
            token_elevated: false,
            image_path: r"C:\Hane\hane.exe".to_owned(),
        }
    }

    fn expected_child() -> ExpectedStandardChild {
        ExpectedStandardChild {
            pid: 500,
            session_id: 1,
            token_sid: "S-1-5-21-1".to_owned(),
            image_path: r"\\?\c:\hane\HANE.exe".to_owned(),
        }
    }

    #[test]
    fn the_new_version_is_accepted_only_with_an_unelevated_token_of_the_user() {
        verify_standard_child(&child_facts(), &expected_child()).unwrap();

        let mut inherited_admin = child_facts();
        inherited_admin.token_elevated = true;
        let mut other_process = child_facts();
        other_process.pid = 501;
        let mut other_session = child_facts();
        other_session.session_id = 2;
        let mut other_account = child_facts();
        other_account.token_sid = "S-1-5-21-2".to_owned();
        let mut other_image = child_facts();
        other_image.image_path = r"C:\Hane\evil.exe".to_owned();
        let mut unknown_image = child_facts();
        unknown_image.image_path = String::new();
        for forged in [inherited_admin, other_process, other_session, other_account, other_image, unknown_image] {
            assert_eq!(
                verify_standard_child(&forged, &expected_child()).unwrap_err().code,
                ErrorCode::IdentityMismatch,
                "{forged:?}"
            );
        }
    }

    #[test]
    fn a_standard_token_must_be_the_users_and_unelevated() {
        require_standard_token("S-1-5-21-1", false, "S-1-5-21-1").unwrap();
        for (sid, elevated, expected) in [
            ("S-1-5-21-1", true, "S-1-5-21-1"),
            ("S-1-5-21-2", false, "S-1-5-21-1"),
            ("", false, ""),
        ] {
            assert!(require_standard_token(sid, elevated, expected).is_err());
        }
    }

    #[test]
    fn a_nonce_is_valid_once_for_its_own_transaction() {
        let mut ledger = NonceLedger::default();
        let nonce = Nonce::generate().unwrap();
        let other = Nonce::generate().unwrap();
        ledger.issue("txn-a", &nonce);
        assert!(ledger.consume("txn-b", nonce.as_str()).is_err());
        assert!(ledger.consume("txn-a", other.as_str()).is_err());
        ledger.consume("txn-a", nonce.as_str()).unwrap();
        assert!(ledger.consume("txn-a", nonce.as_str()).is_err(), "replay");
    }

    struct FakeProcess;

    impl ElevatedProcess for FakeProcess {
        fn pid(&self) -> u32 {
            77
        }

        fn wait_exit(&mut self, _timeout: Duration) -> Option<u32> {
            None
        }

        fn terminate(&mut self) {}
    }

    struct FakeLauncher(std::result::Result<(), LaunchFailure>);

    impl ElevatedLauncher for FakeLauncher {
        type Process = FakeProcess;

        fn launch(&mut self, _helper: &Path) -> std::result::Result<FakeProcess, LaunchFailure> {
            self.0.clone().map(|()| FakeProcess)
        }
    }

    fn failure(os_error: Option<u32>) -> LaunchFailure {
        LaunchFailure { stage: "launch", api: "ShellExecuteExW", os_error, path: "C:\\h\\hane-updater.exe".to_owned() }
    }

    #[test]
    fn a_declined_prompt_is_a_japanese_cancellation_and_retryable() {
        let mut launcher = FakeLauncher(Err(failure(Some(ERROR_CANCELLED))));
        let err = launch_elevated(&mut launcher, Path::new("h")).err().unwrap();
        assert!(is_declined(&err));
        assert!(err.detail.contains("現在のHane"));
        // The seam is reusable: a later attempt is not blocked by the refusal.
        let mut again = FakeLauncher(Ok(()));
        assert_eq!(launch_elevated(&mut again, Path::new("h")).ok().unwrap().pid(), 77);
    }

    #[test]
    fn a_740_on_the_normal_start_is_kept_as_a_fact_and_is_not_an_elevation_request() {
        let path = Path::new("C:\\h\\hane-updater.exe");
        let required = normal_start_error(Some(740), path);
        assert!(!is_elevation_required(&required));
        assert_eq!(elevation_message(&required), None);
        for fact in ["os_error=740", "stage=normal-launch", "hane-updater.exe"] {
            assert!(required.detail.contains(fact), "{fact} in {}", required.detail);
        }
        assert!(is_elevation_required(&protected_placement_error()));
        assert_eq!(elevation_message(&protected_placement_error()), Some(ELEVATION_UNAVAILABLE_MESSAGE));
        for other in [Some(5), Some(1223), None] {
            let err = normal_start_error(other, path);
            assert!(!is_elevation_required(&err));
            assert_eq!(elevation_message(&err), None);
        }
    }

    #[test]
    fn other_launch_failures_keep_their_facts_and_are_not_a_cancellation() {
        let mut launcher = FakeLauncher(Err(failure(Some(5))));
        let err = launch_elevated(&mut launcher, Path::new("h")).err().unwrap();
        assert!(!is_declined(&err));
        for fact in ["stage=launch", "api=ShellExecuteExW", "os_error=5", "hane-updater.exe"] {
            assert!(err.detail.contains(fact), "{fact} in {}", err.detail);
        }
    }

    // ------------------------------------------------------ connect_elevated

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    const HELPER_PID: u32 = 77;
    const SESSION: u32 = 1;
    const USER: &str = "S-1-5-21-1";

    type Events = Arc<Mutex<Vec<&'static str>>>;

    struct Script {
        available: bool,
        owner: String,
        launch: std::result::Result<(), LaunchFailure>,
        /// `None`: nobody connects.
        peer: Option<PeerFacts>,
    }

    fn genuine() -> PeerFacts {
        PeerFacts { pid: HELPER_PID, session_id: SESSION, token_sid: USER.to_owned(), token_elevated: true }
    }

    fn script() -> Script {
        Script { available: true, owner: USER.to_owned(), launch: Ok(()), peer: Some(genuine()) }
    }

    struct ScriptedProcess {
        events: Events,
        stopped: Arc<AtomicBool>,
    }

    impl ElevatedProcess for ScriptedProcess {
        fn pid(&self) -> u32 {
            HELPER_PID
        }

        fn wait_exit(&mut self, _timeout: Duration) -> Option<u32> {
            self.stopped.load(Ordering::SeqCst).then_some(0)
        }

        fn terminate(&mut self) {
            self.events.lock().unwrap().push("terminate");
            self.stopped.store(true, Ordering::SeqCst);
        }
    }

    struct ScriptedListener {
        peer: Option<PeerFacts>,
    }

    impl ElevatedListener for ScriptedListener {
        fn accept(
            &mut self,
            _timeout: Duration,
            _helper_alive: &mut dyn FnMut() -> bool,
        ) -> Result<AcceptedPeer> {
            match self.peer.take() {
                Some(facts) => Ok(AcceptedPeer {
                    facts,
                    reader: Box::new(std::io::empty()),
                    writer: Box::new(std::io::sink()),
                }),
                None => Err(UpdateError::new(ErrorCode::Timeout, "the elevated helper did not connect")),
            }
        }
    }

    struct ScriptedOs {
        script: Mutex<Script>,
        events: Events,
        stopped: Arc<AtomicBool>,
    }

    impl ElevationOs for ScriptedOs {
        fn standard_restart_available(&self) -> bool {
            self.script.lock().unwrap().available
        }

        fn placement_access(&self, _install_root: &Path) -> PlacementAccess {
            PlacementAccess { protected: false, write: WriteAccess::Unknown }
        }

        fn current_principals(&self, _install_root: &Path) -> Result<(String, String)> {
            Ok((USER.to_owned(), self.script.lock().unwrap().owner.clone()))
        }

        fn session_id(&self) -> Result<u32> {
            Ok(SESSION)
        }

        fn listen(&self, _transaction: &TransactionId) -> Result<Box<dyn ElevatedListener>> {
            self.events.lock().unwrap().push("listen");
            Ok(Box::new(ScriptedListener { peer: self.script.lock().unwrap().peer.clone() }))
        }

        fn launch(&self, _helper: &Path) -> std::result::Result<Box<dyn ElevatedProcess>, LaunchFailure> {
            self.events.lock().unwrap().push("launch");
            self.script.lock().unwrap().launch.clone().map(|()| {
                Box::new(ScriptedProcess {
                    events: Arc::clone(&self.events),
                    stopped: Arc::clone(&self.stopped),
                }) as Box<dyn ElevatedProcess>
            })
        }
    }

    fn connect(script: Script) -> (std::result::Result<ElevatedConnection, ConnectFailure>, Vec<&'static str>) {
        let events: Events = Arc::new(Mutex::new(Vec::new()));
        let os = ScriptedOs {
            script: Mutex::new(script),
            events: Arc::clone(&events),
            stopped: Arc::new(AtomicBool::new(false)),
        };
        let transaction = TransactionId::generate().unwrap();
        let result = connect_elevated(
            &os,
            Path::new("C:\\hane"),
            Path::new("C:\\h\\hane-updater.exe"),
            &transaction,
            Duration::from_secs(1),
        );
        let seen = events.lock().unwrap().clone();
        (result, seen)
    }

    #[test]
    fn the_route_is_closed_before_any_prompt_while_the_standard_restart_is_missing() {
        let (result, events) = connect(Script { available: false, ..script() });
        let failure = result.err().unwrap();
        assert!(is_elevation_required(&failure.error));
        assert!(failure.helper_stopped);
        assert!(events.is_empty(), "no endpoint and no prompt: {events:?}");
    }

    #[test]
    fn an_unverifiable_install_owner_is_refused_before_any_prompt() {
        let (result, events) = connect(Script { owner: "S-1-5-21-9".to_owned(), ..script() });
        let failure = result.err().unwrap();
        assert_eq!(failure.error.code, ErrorCode::IdentityMismatch);
        assert!(failure.helper_stopped);
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_declined_prompt_stops_before_any_helper_exists_and_can_be_retried() {
        let (result, events) = connect(Script { launch: Err(failure(Some(ERROR_CANCELLED))), ..script() });
        let refused = result.err().unwrap();
        assert!(is_declined(&refused.error));
        assert!(refused.helper_stopped);
        assert_eq!(events, ["listen", "launch"]);

        let (again, events) = connect(script());
        assert!(again.is_ok());
        assert_eq!(events, ["listen", "launch"]);
    }

    #[test]
    fn only_the_launched_elevated_process_of_the_requesting_account_is_accepted() {
        let (result, events) = connect(script());
        let connection = result.ok().unwrap();
        assert_eq!(connection.process.pid(), HELPER_PID);
        assert!(!events.contains(&"terminate"));

        let mut other_process = genuine();
        other_process.pid = HELPER_PID + 1;
        let mut other_session = genuine();
        other_session.session_id = SESSION + 1;
        let mut other_account = genuine();
        other_account.token_sid = "S-1-5-21-2".to_owned();
        let mut not_elevated = genuine();
        not_elevated.token_elevated = false;
        for forged in [other_process, other_session, other_account, not_elevated] {
            let (result, events) = connect(Script { peer: Some(forged), ..script() });
            let refused = result.err().unwrap();
            assert_eq!(refused.error.code, ErrorCode::IdentityMismatch);
            assert!(refused.helper_stopped, "the launched process was stopped and confirmed");
            assert_eq!(events.last(), Some(&"terminate"));
        }
    }

    #[test]
    fn a_helper_that_never_connects_is_stopped() {
        let (result, events) = connect(Script { peer: None, ..script() });
        let refused = result.err().unwrap();
        assert_eq!(refused.error.code, ErrorCode::Timeout);
        assert!(refused.helper_stopped);
        assert_eq!(events.last(), Some(&"terminate"));
    }

    #[test]
    fn the_unavailable_os_never_launches_anything() {
        let os = UnavailableElevation;
        assert!(!os.standard_restart_available());
        assert!(os.listen(&TransactionId::generate().unwrap()).is_err());
        assert!(os.launch(Path::new("h")).is_err());
    }
}
