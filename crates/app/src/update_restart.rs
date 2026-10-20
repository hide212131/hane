//! The application side of "restart and update" (Issue #510, U4).
//!
//! *Before the quit.* [`start_helper`] turns a Ready candidate into a started
//! helper that has answered `Prepared`; it does not stop the AI service, does
//! not authorize the helper's exit and does not quit. [`HelperHandoff`] is what
//! the window then holds: it writes the restart ticket (after re-confirming the
//! candidate, the placement and the freshness of the restart permit), and only
//! its `authorize_exit` lets the helper proceed. If anything fails the
//! application stays open and untouched; dropping the handoff aborts the
//! helper.
//!
//! The Ready candidate is copied into the helper's own transaction directory
//! here; the helper is never given a path and re-verifies the copy itself.
//!
//! *After the commit.* [`restore_after_commit`] takes the ticket of the
//! transaction that just committed (once, and only if its journal is durably
//! `Committed`) and [`plan_restore`] turns it into the files to open through the
//! ordinary open path. A saved file that was deleted or moved is skipped; it
//! never stops the others.

use std::path::{Path, PathBuf};

use hane_ui::context_menu::{self, ExplorerRegistration};
use hane_ui::{ExplorerRepairScope, PreparedUpdate, RestartTicketSnapshot};
use hane_update::helper::{PreparedRestart, prepare_restart};
use hane_update::identity::TransactionId;
use hane_update::platform::Placement;
use hane_update::platform::lease::Coordination;
use hane_update::state_root::{
    self, Cleanup, ExplorerTicket, MAX_POSITION, MAX_TICKET_FILES, RestartTicket, TicketContent,
    TicketFile, TicketRestore, ticket_path_ok,
};
use hane_update::trust::TrustPolicy;
use hane_update::windows_apply::CandidateSource;
use hane_update::{ErrorCode, UpdateError};

use crate::app_update::{ARCHIVE_FILE, CATALOG_FILE, SIGNATURE_FILE};

/// `candidate_dir` is the Ready candidate directory, which is named after
/// `candidate_id`; any other directory is refused. `state_root` is the running
/// application's own `FileStateStore::root()`; the new version uses exactly
/// that root, never a default it resolves itself.
pub fn start_helper(
    placement: &Placement,
    policy: &TrustPolicy,
    candidate_id: &str,
    candidate_dir: &Path,
    state_root: &Path,
) -> Result<PreparedRestart, UpdateError> {
    if candidate_dir.file_name().and_then(|name| name.to_str()) != Some(candidate_id) {
        return Err(UpdateError::new(
            ErrorCode::IdentityMismatch,
            "the candidate directory is not the requested candidate",
        ));
    }
    prepare_restart(
        placement,
        policy,
        candidate_id,
        &CandidateSource {
            catalog: &candidate_dir.join(CATALOG_FILE),
            signature: &candidate_dir.join(SIGNATURE_FILE),
            archive: &candidate_dir.join(ARCHIVE_FILE),
        },
        state_root,
    )
}

// ---------------------------------------------------------------- handoff

/// The helper as the handoff needs it, so the order of its steps can be tested
/// without starting a process.
pub(crate) trait HelperSession: Send {
    fn transaction(&self) -> TransactionId;
    fn authorize_exit(self) -> Result<(), UpdateError>;
    fn abort(self);
}

impl HelperSession for PreparedRestart {
    fn transaction(&self) -> TransactionId {
        PreparedRestart::transaction(self).clone()
    }

    fn authorize_exit(self) -> Result<(), UpdateError> {
        PreparedRestart::authorize_exit(self)
    }

    fn abort(self) {
        PreparedRestart::abort(self);
    }
}

/// A prepared helper together with what is needed to write its ticket.
pub(crate) struct HelperHandoff<S: HelperSession> {
    session: Option<S>,
    coordination: Coordination,
    state_root: PathBuf,
    /// Re-confirms the Ready candidate and the install placement. Asked after
    /// the ticket was validated and before it is written.
    still_valid: Box<dyn Fn() -> Result<(), String> + Send>,
    /// Reads the enabled Explorer integration before the update. A failure
    /// keeps the ticket from being written.
    explorer: Box<dyn Fn() -> Result<Option<ExplorerTicket>, String> + Send>,
}

impl<S: HelperSession> HelperHandoff<S> {
    pub(crate) fn new(
        session: S,
        coordination: Coordination,
        state_root: PathBuf,
        still_valid: Box<dyn Fn() -> Result<(), String> + Send>,
    ) -> Self {
        Self {
            session: Some(session),
            coordination,
            state_root,
            still_valid,
            explorer: Box::new(capture_explorer),
        }
    }

    #[cfg(test)]
    fn with_explorer(
        mut self,
        explorer: Box<dyn Fn() -> Result<Option<ExplorerTicket>, String> + Send>,
    ) -> Self {
        self.explorer = explorer;
        self
    }
}

/// The Explorer integration the user has ON, read by the trusted inspection.
fn capture_explorer() -> Result<Option<ExplorerTicket>, String> {
    context_menu::capture_explorer_registration()
        .map(|registration| registration.map(explorer_ticket))
        .map_err(|err| format!("Explorer 連携の現在の状態を確認できません: {err}"))
}

fn explorer_ticket(registration: ExplorerRegistration) -> ExplorerTicket {
    ExplorerTicket {
        owner: registration.owner,
        package_name: registration.package_name,
        publisher: registration.publisher,
        file_enabled: registration.file_enabled,
        folder_enabled: registration.folder_enabled,
    }
}

fn explorer_registration(ticket: &ExplorerTicket) -> ExplorerRegistration {
    ExplorerRegistration {
        owner: ticket.owner.clone(),
        package_name: ticket.package_name.clone(),
        publisher: ticket.publisher.clone(),
        file_enabled: ticket.file_enabled,
        folder_enabled: ticket.folder_enabled,
    }
}

impl<S: HelperSession + 'static> PreparedUpdate for HelperHandoff<S> {
    fn issue_ticket(
        &mut self,
        ticket: &RestartTicketSnapshot,
        permit_is_fresh: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| "the update helper is gone".to_owned())?;
        let transaction = session.transaction();
        let still_valid = &self.still_valid;
        let mut content = ticket_content(ticket);
        content.explorer = (self.explorer)()?;
        state_root::issue_ticket(
            &self.coordination,
            &transaction,
            &self.state_root,
            content,
            || {
                still_valid().map_err(|why| UpdateError::new(ErrorCode::IdentityMismatch, why))?;
                if permit_is_fresh() {
                    Ok(())
                } else {
                    Err(UpdateError::new(
                        ErrorCode::TransitionInvalid,
                        "the restart permit is stale",
                    ))
                }
            },
        )
        .map_err(|err| err.to_string())
    }

    fn authorize_exit(mut self: Box<Self>) -> Result<(), String> {
        match self.session.take() {
            Some(session) => session.authorize_exit().map_err(|err| err.to_string()),
            None => Err("the update helper is gone".to_owned()),
        }
    }

    fn abort(mut self: Box<Self>) {
        if let Some(session) = self.session.take() {
            state_root::discard_ticket(&self.coordination, &session.transaction());
            session.abort();
        }
    }
}

fn ticket_text(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    ticket_path_ok(text).then(|| text.to_owned())
}

/// The ticket content for the saved files of the window. Entries a ticket
/// cannot carry (a path that is not text, too long or not absolute) and files
/// beyond its limit are left out; they are reopened by the user, never lost,
/// since they are saved documents.
pub(crate) fn ticket_content(snapshot: &RestartTicketSnapshot) -> TicketContent {
    let active_path = snapshot
        .active
        .and_then(|index| snapshot.files.get(index))
        .map(|file| &file.path);
    let mut files = Vec::new();
    let mut active = None;
    for file in &snapshot.files {
        if files.len() >= MAX_TICKET_FILES {
            break;
        }
        let Some(path) = ticket_text(&file.path) else {
            continue;
        };
        if Some(&file.path) == active_path {
            active = u32::try_from(files.len()).ok();
        }
        files.push(TicketFile {
            path,
            scroll_line: file.scroll_line.min(MAX_POSITION),
            caret_offset: file.caret_offset.min(MAX_POSITION),
        });
    }
    TicketContent {
        work_folder: snapshot.work_folder.as_deref().and_then(ticket_text),
        files,
        active,
        explorer: None,
    }
}

// ---------------------------------------------------------------- restore

/// What the new version opens after the commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestorePlan {
    /// The work folder to open, when it still exists.
    pub work_folder: Option<PathBuf>,
    /// Saved files in the order to open them; the active file is last so it
    /// ends up shown.
    pub files: Vec<PathBuf>,
    /// Saved files that no longer exist (deleted or moved).
    pub missing: Vec<PathBuf>,
    /// Why the Explorer integration could not be carried to the new version.
    /// `None` when there was nothing to carry or it was carried. Shown to the
    /// user: the update itself did succeed.
    pub explorer_failure: Option<String>,
    /// The registrations that were enabled before the update; the repair
    /// guidance names only these. Meaningful only with `explorer_failure`.
    pub explorer_repair: ExplorerRepairScope,
}

/// Turns a ticket into a plan. A file that is gone is recorded and skipped.
pub fn plan_restore(
    ticket: &RestartTicket,
    is_file: impl Fn(&Path) -> bool,
    is_dir: impl Fn(&Path) -> bool,
) -> RestorePlan {
    let work_folder = ticket
        .work_folder
        .as_deref()
        .map(PathBuf::from)
        .filter(|folder| is_dir(folder));
    let active = ticket
        .active
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| ticket.files.get(index))
        .map(|file| PathBuf::from(&file.path));
    let mut files = Vec::new();
    let mut missing = Vec::new();
    let mut active_present = false;
    for file in &ticket.files {
        let path = PathBuf::from(&file.path);
        if !is_file(&path) {
            missing.push(path);
        } else if Some(&path) == active.as_ref() {
            active_present = true;
        } else if !files.contains(&path) {
            files.push(path);
        }
    }
    if active_present && let Some(active) = active {
        files.retain(|path| *path != active);
        files.push(active);
    }
    RestorePlan {
        work_folder,
        files,
        missing,
        explorer_failure: None,
        explorer_repair: ExplorerRepairScope::default(),
    }
}

const CLEANUP_ATTEMPTS: usize = 3;

/// Takes and removes the ticket after a durable commit. `None` when there is
/// nothing to restore, the ticket was already used, or it is unusable; an
/// unusable ticket never blocks the start.
pub fn restore_after_commit(restore: &TicketRestore) -> Option<RestorePlan> {
    restore_after_commit_with(restore, |saved| {
        context_menu::migrate_explorer_registration(&explorer_registration(saved))
    })
}

/// [`restore_after_commit`] with the Explorer migration as a seam. The
/// migration runs only for a ticket this call claimed after the durable
/// commit. A failure is reported and leaves the integration as the ordinary
/// inspection sees it (Stale), never as a success; it does not stop the
/// document restore.
pub(crate) fn restore_after_commit_with(
    restore: &TicketRestore,
    migrate_explorer: impl FnOnce(&ExplorerTicket) -> std::io::Result<()>,
) -> Option<RestorePlan> {
    let ticket = match restore.take() {
        Ok(Some(ticket)) => ticket,
        Ok(None) => return None,
        Err(err) => {
            eprintln!("Hane did not restore the files of the update restart ({err}).");
            return None;
        }
    };
    let explorer_failure = carry_explorer(ticket.explorer.as_ref(), migrate_explorer);
    let mut plan = plan_restore(&ticket, |path| path.is_file(), |path| path.is_dir());
    if explorer_failure.is_some()
        && let Some(saved) = ticket.explorer.as_ref()
    {
        plan.explorer_repair = ExplorerRepairScope {
            file: saved.file_enabled,
            folder: saved.folder_enabled,
        };
    }
    plan.explorer_failure = explorer_failure;
    finish_cleanup(restore);
    Some(plan)
}

/// Runs the Explorer migration for a ticket that carries one. The failure
/// reason is returned for the user-facing notice (and logged); nothing to carry
/// or a successful migration is `None`.
fn carry_explorer(
    saved: Option<&ExplorerTicket>,
    migrate: impl FnOnce(&ExplorerTicket) -> std::io::Result<()>,
) -> Option<String> {
    let saved = saved?;
    match migrate(saved) {
        Ok(()) => None,
        Err(err) => {
            eprintln!("Hane did not carry the Explorer integration over the update ({err}).");
            Some(err.to_string())
        }
    }
}

/// The ticket is already used when this runs, so a failure only leaves the
/// harmless claimed file behind (`cleanup_pending`); it is retried a few times.
fn finish_cleanup(restore: &TicketRestore) {
    for _ in 0..CLEANUP_ATTEMPTS {
        if restore.finish() == Cleanup::Done {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    eprintln!("cleanup_pending: the used restart ticket could not be removed yet.");
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use hane_ui::RestartTicketFile;
    use hane_update::identity::{InstallFacts, InstallId};
    use hane_update::testing::TempDir;

    use super::*;

    fn abs(name: &str) -> PathBuf {
        std::env::temp_dir().join("hane-restart-test").join(name)
    }

    fn file(name: &str) -> RestartTicketFile {
        RestartTicketFile {
            path: abs(name),
            scroll_line: 7,
            caret_offset: 11,
        }
    }

    #[test]
    fn the_ticket_carries_only_saved_paths_and_bounded_positions() {
        let snapshot = RestartTicketSnapshot {
            work_folder: Some(abs("work")),
            files: vec![file("a.md"), file("b.md")],
            active: Some(1),
        };
        let content = ticket_content(&snapshot);
        assert_eq!(content.files.len(), 2);
        assert_eq!(content.active, Some(1));
        assert_eq!(content.files[0].scroll_line, 7);
        assert_eq!(content.work_folder.as_deref(), abs("work").to_str());
    }

    #[test]
    fn what_a_ticket_cannot_carry_is_left_out_without_failing_the_rest() {
        let mut far = file("far.md");
        far.scroll_line = u32::MAX;
        let snapshot = RestartTicketSnapshot {
            work_folder: Some(PathBuf::from("relative")),
            files: vec![
                RestartTicketFile {
                    path: PathBuf::from("relative.md"),
                    scroll_line: 0,
                    caret_offset: 0,
                },
                RestartTicketFile {
                    path: abs(&"x".repeat(5000)),
                    scroll_line: 0,
                    caret_offset: 0,
                },
                far,
                file("kept.md"),
            ],
            active: Some(3),
        };
        let content = ticket_content(&snapshot);
        assert_eq!(content.work_folder, None);
        assert_eq!(content.files.len(), 2);
        assert_eq!(content.files[0].scroll_line, MAX_POSITION);
        assert_eq!(content.active, Some(1), "the active index follows the kept entries");
    }

    #[test]
    fn more_files_than_a_ticket_holds_are_cut_and_a_cut_active_file_is_dropped() {
        let files: Vec<_> = (0..MAX_TICKET_FILES + 5).map(|n| file(&format!("{n}.md"))).collect();
        let snapshot = RestartTicketSnapshot {
            work_folder: None,
            files,
            active: Some(MAX_TICKET_FILES + 2),
        };
        let content = ticket_content(&snapshot);
        assert_eq!(content.files.len(), MAX_TICKET_FILES);
        assert_eq!(content.active, None);
    }

    fn ticket(files: &[&str], active: Option<u32>, work_folder: Option<&str>) -> RestartTicket {
        RestartTicket {
            schema: state_root::TICKET_SCHEMA,
            install_id: "i".to_owned(),
            transaction_id: "t".to_owned(),
            state_root: "/s".to_owned(),
            work_folder: work_folder.map(str::to_owned),
            files: files
                .iter()
                .map(|path| TicketFile {
                    path: (*path).to_owned(),
                    scroll_line: 0,
                    caret_offset: 0,
                })
                .collect(),
            active,
            explorer: None,
        }
    }

    #[test]
    fn a_missing_file_is_skipped_and_the_rest_is_restored_with_the_active_one_last() {
        let ticket = ticket(&["/w/a.md", "/w/gone.md", "/w/b.md", "/w/c.md"], Some(0), Some("/w"));
        let plan = plan_restore(&ticket, |path| path != Path::new("/w/gone.md"), |_| true);
        assert_eq!(plan.work_folder, Some(PathBuf::from("/w")));
        assert_eq!(
            plan.files,
            [
                PathBuf::from("/w/b.md"),
                PathBuf::from("/w/c.md"),
                PathBuf::from("/w/a.md")
            ]
        );
        assert_eq!(plan.missing, [PathBuf::from("/w/gone.md")]);
    }

    #[test]
    fn a_missing_active_file_and_work_folder_still_restore_the_others() {
        let ticket = ticket(&["/w/a.md", "/w/b.md"], Some(0), Some("/w"));
        let plan = plan_restore(&ticket, |path| path == Path::new("/w/b.md"), |_| false);
        assert_eq!(plan.work_folder, None);
        assert_eq!(plan.files, [PathBuf::from("/w/b.md")]);
        assert_eq!(plan.missing, [PathBuf::from("/w/a.md")]);
    }

    #[test]
    fn no_files_exist_means_an_empty_plan() {
        let ticket = ticket(&["/w/a.md"], None, None);
        let plan = plan_restore(&ticket, |_| false, |_| false);
        assert!(plan.files.is_empty());
        assert_eq!(plan.missing.len(), 1);
    }

    // ----------------------------------------------------------- handoff

    struct FakeSession {
        id: TransactionId,
        log: Arc<Mutex<Vec<&'static str>>>,
        fail_exit: bool,
    }

    impl HelperSession for FakeSession {
        fn transaction(&self) -> TransactionId {
            self.id.clone()
        }

        fn authorize_exit(self) -> Result<(), UpdateError> {
            self.log.lock().unwrap().push("authorize");
            if self.fail_exit {
                Err(UpdateError::new(ErrorCode::Io, "pipe closed"))
            } else {
                Ok(())
            }
        }

        fn abort(self) {
            self.log.lock().unwrap().push("abort");
        }
    }

    fn handoff(
        dir: &TempDir,
        fail_exit: bool,
        valid: Result<(), String>,
    ) -> (HelperHandoff<FakeSession>, Arc<Mutex<Vec<&'static str>>>) {
        let install = InstallId::derive(&InstallFacts {
            os: "test",
            volume_id: "v",
            install_path: "/x",
            owner: "u",
        })
        .unwrap();
        let coordination = Coordination::create(dir.path(), &install).unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let session = FakeSession {
            id: TransactionId::generate().unwrap(),
            log: Arc::clone(&log),
            fail_exit,
        };
        let handoff = HelperHandoff::new(
            session,
            coordination,
            dir.path().join("state"),
            Box::new(move || valid.clone()),
        );
        (handoff, log)
    }

    #[test]
    fn authorizing_reaches_the_helper_and_a_failed_authorization_is_reported() {
        let dir = TempDir::new("handoff-exit");
        let (ok, log) = handoff(&dir, false, Ok(()));
        Box::new(ok).authorize_exit().unwrap();
        assert_eq!(*log.lock().unwrap(), ["authorize"]);

        let (failing, log) = handoff(&dir, true, Ok(()));
        let err = Box::new(failing).authorize_exit().unwrap_err();
        assert!(err.contains("pipe closed"));
        assert_eq!(*log.lock().unwrap(), ["authorize"]);
    }

    #[test]
    fn aborting_stops_the_helper_without_authorizing_it() {
        let dir = TempDir::new("handoff-abort");
        let (handoff, log) = handoff(&dir, false, Ok(()));
        Box::new(handoff).abort();
        assert_eq!(*log.lock().unwrap(), ["abort"]);
    }

    #[test]
    fn a_ticket_is_not_written_for_a_changed_candidate_stale_permit_or_missing_binding() {
        let dir = TempDir::new("handoff-ticket");
        let snapshot = RestartTicketSnapshot {
            work_folder: None,
            files: vec![file("a.md")],
            active: Some(0),
        };
        // The state root is not bound for this transaction: no ticket.
        let (mut unbound, _log) = handoff(&dir, false, Ok(()));
        assert!(unbound.issue_ticket(&snapshot, &|| true).is_err());

        // The candidate or the placement changed: no ticket.
        let (mut changed, _log) = handoff(&dir, false, Err("candidate changed".to_owned()));
        assert!(changed.issue_ticket(&snapshot, &|| true).is_err());

        // The permit went stale: no ticket.
        let (mut stale, _log) = handoff(&dir, false, Ok(()));
        assert!(stale.issue_ticket(&snapshot, &|| false).is_err());
    }

    #[test]
    fn an_unreadable_explorer_state_keeps_the_ticket_from_being_written() {
        let dir = TempDir::new("handoff-explorer");
        let snapshot = RestartTicketSnapshot {
            work_folder: None,
            files: vec![file("a.md")],
            active: Some(0),
        };
        let (handoff, _log) = handoff(&dir, false, Ok(()));
        let mut handoff =
            handoff.with_explorer(Box::new(|| Err("Explorer 連携の状態を確認できません".to_owned())));
        let err = handoff.issue_ticket(&snapshot, &|| true).unwrap_err();
        assert!(err.contains("Explorer"));
    }

    #[test]
    fn the_explorer_registration_converts_without_losing_owner_or_verbs() {
        for (file_enabled, folder_enabled) in [(true, true), (true, false), (false, true)] {
            let registration = ExplorerRegistration {
                owner: "owner".to_owned(),
                package_name: "Hane.ShellIntegration".to_owned(),
                publisher: "CN=Hane".to_owned(),
                file_enabled,
                folder_enabled,
            };
            let ticket = explorer_ticket(registration.clone());
            assert_eq!(ticket.owner, "owner");
            assert_eq!(ticket.file_enabled, file_enabled);
            assert_eq!(ticket.folder_enabled, folder_enabled);
            assert_eq!(explorer_registration(&ticket), registration);
        }
    }

    #[test]
    fn a_migration_failure_is_reported_and_a_success_or_nothing_is_not() {
        let saved = explorer_ticket(ExplorerRegistration {
            owner: "owner".to_owned(),
            package_name: "Hane.ShellIntegration".to_owned(),
            publisher: "CN=Hane".to_owned(),
            file_enabled: true,
            folder_enabled: false,
        });
        let ran = std::cell::Cell::new(0);
        assert_eq!(
            carry_explorer(Some(&saved), |_| {
                ran.set(ran.get() + 1);
                Ok(())
            }),
            None
        );
        assert_eq!(ran.get(), 1);
        let failure = carry_explorer(Some(&saved), |_| Err(std::io::Error::other("登録に失敗")));
        assert!(failure.expect("a failure is reported").contains("登録に失敗"));
        assert_eq!(
            carry_explorer(None, |_| panic!("nothing to carry is never migrated")),
            None
        );
    }

    #[test]
    fn the_document_ticket_content_carries_no_explorer_state_by_itself() {
        let snapshot = RestartTicketSnapshot {
            work_folder: None,
            files: vec![file("a.md")],
            active: Some(0),
        };
        assert_eq!(ticket_content(&snapshot).explorer, None);
    }
}
