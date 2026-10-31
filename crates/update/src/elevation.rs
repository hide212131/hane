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

use std::collections::BTreeSet;
use std::time::Duration;

use crate::error::{ErrorCode, Result, UpdateError};
use crate::protocol::Nonce;

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

fn is_protected_owner(sid: &str) -> bool {
    sid == SID_ADMINISTRATORS || sid == SID_SYSTEM || sid.starts_with(SID_TRUSTED_INSTALLER_PREFIX)
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
pub trait ElevatedProcess {
    fn pid(&self) -> u32;
    /// `Some(code)` once the process has exited within `timeout`.
    fn wait_exit(&mut self, timeout: Duration) -> Option<u32>;
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
    launcher.launch(helper).map_err(|failure| {
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
    })
}

/// Whether `err` is the user's refusal of the consent prompt.
pub fn is_declined(err: &UpdateError) -> bool {
    err.code == ErrorCode::Cancelled && err.detail.starts_with(DECLINED_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
    fn other_launch_failures_keep_their_facts_and_are_not_a_cancellation() {
        let mut launcher = FakeLauncher(Err(failure(Some(5))));
        let err = launch_elevated(&mut launcher, Path::new("h")).err().unwrap();
        assert!(!is_declined(&err));
        for fact in ["stage=launch", "api=ShellExecuteExW", "os_error=5", "hane-updater.exe"] {
            assert!(err.detail.contains(fact), "{fact} in {}", err.detail);
        }
    }
}
