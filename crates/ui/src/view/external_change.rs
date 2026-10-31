//! Follows changes other applications make to the files that are open
//! (Issue #653).
//!
//! The rules live in `hane_session::DocumentSession`: this module only starts
//! the background reads, hands their results back to the session together
//! with the ticket they were started with, and turns the session's answer
//! into what the window shows. A watcher notification is a hint to look
//! again, never an instruction to change the text:
//!
//! - an unedited document is replaced by the new disk version (new document
//!   generation; the caret and scroll position are carried over);
//! - a document with unsaved edits, or with an IME composition, keeps its text
//!   and history, and the window says the file was changed outside;
//! - discarding edits or overwriting the disk version needs a platform
//!   confirmation, and the confirmation is bound to the revision and the
//!   external version that were on screen when it was asked for.
//!
//! The file reads, hashing and comparison all run on the background executor.

use super::*;
use hane_session::{
    ContentFingerprint, ExternalCheckOutcome, ExternalCheckResult, ExternalCheckStart,
    ExternalCheckTicket, ExternalStatus, ReloadOutcome, ReloadVerifyOutcome, ReloadVerifyRequest,
    ReloadVerifyResult, ResolutionKind, run_external_check, run_reload_verification,
};

/// Prefix of the save-problem text that reports an unreadable external file,
/// so a later successful check can clear exactly that problem and no other.
const UNVERIFIABLE_PREFIX: &str = "外部のファイルを確認できません: ";

/// How long a deferred reload (input, IME or a write was in progress) waits
/// before the file is checked again.
const DEFERRED_RECHECK_DELAY: Duration = Duration::from_millis(1_000);

/// The ways the footer offers to resolve an external change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResolutionAction {
    SaveAs,
    Reload,
    Overwrite,
    /// Looks at the file on disk again; offered while its changes are not
    /// being watched. Changes nothing by itself.
    Recheck,
    /// Shows the disk version and the edited version side by side, read-only.
    Compare,
}

/// Which of the destructive choices apply to the active document right now.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ResolutionChoices {
    pub(super) reload: bool,
    pub(super) overwrite: bool,
}

/// The view state that must survive a document replacement unchanged: the
/// reload is not a navigation, so it must not move the user.
#[derive(Clone, Copy)]
struct ViewPosition {
    scroll_y: f32,
    sidebar_focus: SidebarFocus,
}

impl EditorView {
    /// The choices to offer for the active document, or `None` when it has no
    /// external change to resolve.
    pub(super) fn active_resolution_choices(&self) -> Option<ResolutionChoices> {
        let session = self.sessions.active();
        match session.external_status() {
            ExternalStatus::Changed => Some(ResolutionChoices {
                reload: true,
                overwrite: true,
            }),
            ExternalStatus::Missing if session.path().is_some() => Some(ResolutionChoices {
                reload: false,
                overwrite: true,
            }),
            _ => None,
        }
    }

    fn view_position(&self) -> ViewPosition {
        ViewPosition {
            scroll_y: self.scroll_y,
            sidebar_focus: self.sidebar_focus,
        }
    }

    /// Looks at every open file once. Called when the watcher reports a batch
    /// (including an overflow or a watch error, after which nothing it said
    /// can be trusted to be complete). Untitled documents have no file and
    /// are skipped; files that are not open are never read.
    pub(super) fn check_open_documents_for_external_changes(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<SessionId> = self
            .sessions
            .sessions()
            .filter(|session| session.path().is_some())
            .map(DocumentSession::id)
            .collect();
        for id in ids {
            self.start_external_check(id, cx);
        }
    }

    /// Starts the background read of one document's file. At most one read
    /// runs per document; notifications that arrive meanwhile are folded into
    /// one follow-up by the session.
    pub(super) fn start_external_check(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(instance) = self.document_instance(id) else {
            return;
        };
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let ExternalCheckStart::Run(request) = session.begin_external_check() else {
            return;
        };
        let files = self.files.clone();
        cx.spawn(async move |view, cx| {
            let (ticket, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = run_external_check(files.as_ref(), &request);
                    (request.ticket, result)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_external_check_for(id, instance, ticket, result, cx);
            });
        })
        .detach();
    }

    /// Applies a finished read. The document instance is compared first so a
    /// result for a replaced document or workspace touches nothing; the
    /// session then checks its own ticket (generation, path, request).
    fn finish_external_check_for(
        &mut self,
        id: SessionId,
        instance: DocumentInstance,
        ticket: ExternalCheckTicket,
        result: ExternalCheckResult,
        cx: &mut Context<Self>,
    ) {
        if self.document_instance(id) != Some(instance) {
            return;
        }
        let was_active = self.sessions.active_id() == id;
        let position = self.view_position();
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let outcome = session.finish_external_check(&ticket, result);
        let recheck = session.take_recheck_request();
        let label = session.label();
        let path = session.path().map(Path::to_path_buf);
        let in_sync = session.external_status() == ExternalStatus::InSync;
        match outcome {
            ExternalCheckOutcome::Stale => {}
            ExternalCheckOutcome::Unchanged => {
                if in_sync {
                    self.clear_external_problem(id);
                }
            }
            ExternalCheckOutcome::Reloaded => {
                self.after_document_reloaded(id, was_active, position, cx);
                if was_active {
                    self.status = Some(format!("{label}: 外部の変更を読み込みました"));
                }
            }
            ExternalCheckOutcome::Changed { dirty: true } => {
                self.record_save_problem(id, instance, path, DocumentSaveProblem::Conflict);
            }
            ExternalCheckOutcome::Changed { dirty: false } => {
                // Nothing to lose, but input, a composition or a write was in
                // progress: look again shortly instead of replacing it now.
                self.schedule_external_recheck(id, cx);
            }
            ExternalCheckOutcome::Missing { .. } => {
                // The file is gone: an earlier external conflict / unverifiable
                // notice no longer describes it. A genuine save failure stays.
                self.clear_external_problem(id);
            }
            ExternalCheckOutcome::Unverifiable { reason } => {
                self.record_save_problem(
                    id,
                    instance,
                    path,
                    DocumentSaveProblem::Failed(format!("{UNVERIFIABLE_PREFIX}{reason}")),
                );
            }
        }
        if recheck {
            self.start_external_check(id, cx);
        }
        cx.notify();
    }

    /// Drops the conflict / unverifiable notice of `id`, leaving a genuine
    /// save failure alone.
    fn clear_external_problem(&mut self, id: SessionId) {
        let external = self
            .save_problems
            .get(&id)
            .is_some_and(|scoped| match &scoped.problem {
                DocumentSaveProblem::Conflict => true,
                DocumentSaveProblem::Failed(text) => text.starts_with(UNVERIFIABLE_PREFIX),
                DocumentSaveProblem::SaveAsDialogFailed(_) => false,
            });
        if external {
            self.save_problems.remove(&id);
        }
    }

    fn schedule_external_recheck(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(instance) = self.document_instance(id) else {
            return;
        };
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(DEFERRED_RECHECK_DELAY).await;
            let _ = view.update(cx, |view, cx| {
                if view.document_instance(id) != Some(instance) {
                    return;
                }
                if view
                    .sessions
                    .get(id)
                    .is_some_and(DocumentSession::needs_external_recheck)
                {
                    view.start_external_check(id, cx);
                }
            });
        })
        .detach();
    }

    /// Rebuilds what is derived from the active document after the session
    /// replaced its text, keeping the user where they were. An inactive
    /// document needs nothing here: it is rebuilt when its tab is selected.
    fn after_document_reloaded(
        &mut self,
        id: SessionId,
        was_active: bool,
        position: ViewPosition,
        cx: &mut Context<Self>,
    ) {
        self.clear_external_problem(id);
        if !was_active {
            return;
        }
        if let Some(session) = self.sessions.get_mut(id) {
            session.set_view_state(SessionViewState {
                scroll_y: position.scroll_y,
            });
        }
        self.on_document_replaced();
        // `on_document_replaced` is written for navigation; a reload must not
        // change the mode or move the sidebar focus. The granularity stays at
        // the Lines layout it just installed: Blocks needs the block index and
        // heights, which the background parse publishes (and switches to) once
        // it completes. Restoring Blocks now would draw with an empty index.
        self.sidebar_focus = position.sidebar_focus;
        self.resync_document_find_for_active_document(cx);
        self.schedule_document_parse(cx);
    }

    /// Runs one footer choice for the active document.
    pub(super) fn run_resolution(
        &mut self,
        action: ResolutionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A comparison that is out of date must not lead to a reload, an
        // overwrite or a save-as: compare again first (or close it).
        if matches!(
            action,
            ResolutionAction::SaveAs | ResolutionAction::Reload | ResolutionAction::Overwrite
        ) && self.external_comparison_is_stale()
        {
            self.status = Some(
                "比較中の内容が更新されました。再比較してから操作してください".to_owned(),
            );
            cx.notify();
            return;
        }
        match action {
            ResolutionAction::Compare => self.open_external_comparison(cx),
            ResolutionAction::SaveAs => self.prompt_save_as(cx),
            ResolutionAction::Reload => self.confirm_discarding_reload(window, cx),
            ResolutionAction::Overwrite => self.confirm_overwrite(window, cx),
            ResolutionAction::Recheck => self.recheck_active_document(cx),
        }
    }

    fn confirm_discarding_reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.sessions.active_id();
        let Some(instance) = self.document_instance(id) else {
            return;
        };
        let session = self.sessions.active();
        let Some(shown) = session.observed_fingerprint() else {
            self.status =
                Some("読み直せるディスクの内容がありません。もう一度確認してください".to_owned());
            cx.notify();
            return;
        };
        let revision = session.revision();
        let detail = session.label();
        let receiver = window.prompt(
            gpui::PromptLevel::Warning,
            "未保存の編集を破棄して、ディスクの内容を読み直しますか",
            Some(detail.as_str()),
            &["キャンセル", "破棄して読み直す"],
            cx,
        );
        cx.spawn(async move |view, cx| {
            let confirmed = receiver.await.ok() == Some(1);
            let _ = view.update(cx, |view, cx| {
                view.finish_reload_confirmation(id, instance, revision, shown, confirmed, cx);
            });
        })
        .detach();
    }

    /// Acts on the answer to the discard-and-reload question. A cancel, or an
    /// answer for something that changed since it was asked, changes nothing.
    fn finish_reload_confirmation(
        &mut self,
        id: SessionId,
        instance: DocumentInstance,
        revision: Revision,
        shown: ContentFingerprint,
        confirmed: bool,
        cx: &mut Context<Self>,
    ) {
        if !confirmed || self.document_instance(id) != Some(instance) {
            return;
        }
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        if session.revision() != revision {
            self.status = Some(
                "確認後に文書が編集されたため読み直していません。もう一度操作してください"
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        let Some(approval) =
            session.approve_resolution(ResolutionKind::DiscardAndReload, Some(shown))
        else {
            self.status = Some(
                "確認後にディスクの内容が更新されました。もう一度、内容を確認してください"
                    .to_owned(),
            );
            cx.notify();
            return;
        };
        // The answer is not enough: the disk may have changed while the dialog
        // was open and the watcher result may not have arrived yet. Read the
        // file again in the background and take it only if it is still the
        // version the user was shown.
        let request = match session.begin_confirmed_reload(approval) {
            Ok(request) => request,
            Err(ReloadOutcome::Busy) => {
                self.status = Some(
                    "入力の変換中または保存中のため読み直していません。終わってからやり直してください"
                        .to_owned(),
                );
                cx.notify();
                return;
            }
            Err(_) => {
                self.status = Some(
                    "確認後に文書またはディスクの内容が変わったため読み直していません"
                        .to_owned(),
                );
                cx.notify();
                return;
            }
        };
        let files = self.files.clone();
        cx.spawn(async move |view, cx| {
            let (request, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = run_reload_verification(files.as_ref(), &request);
                    (request, result)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_verified_reload(id, instance, request, result, cx);
            });
        })
        .detach();
    }

    /// Applies the fresh read made after a reload confirmation. Anything but
    /// the exact version the user was shown keeps the text, history and draft
    /// and asks again.
    fn finish_verified_reload(
        &mut self,
        id: SessionId,
        instance: DocumentInstance,
        request: ReloadVerifyRequest,
        result: ReloadVerifyResult,
        cx: &mut Context<Self>,
    ) {
        if self.document_instance(id) != Some(instance) {
            return;
        }
        let was_active = self.sessions.active_id() == id;
        let position = self.view_position();
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        let outcome = session.finish_confirmed_reload(&request, result);
        let dirty = session.is_dirty();
        let path = session.path().map(Path::to_path_buf);
        match outcome {
            ReloadVerifyOutcome::Reloaded => {
                self.after_document_reloaded(id, was_active, position, cx);
                self.status = Some("ディスクの内容を読み直しました".to_owned());
            }
            ReloadVerifyOutcome::Stale => {
                self.status = Some(
                    "確認後に文書またはディスクの内容が変わったため読み直していません"
                        .to_owned(),
                );
            }
            ReloadVerifyOutcome::Busy => {
                self.status = Some(
                    "入力の変換中または保存中のため読み直していません。終わってからやり直してください"
                        .to_owned(),
                );
            }
            ReloadVerifyOutcome::DiskChanged => {
                if dirty {
                    self.record_save_problem(id, instance, path, DocumentSaveProblem::Conflict);
                }
                self.status = Some(
                    "確認後にディスクの内容が更新されたため読み直していません。最新の内容を確認してください"
                        .to_owned(),
                );
            }
            ReloadVerifyOutcome::Missing => {
                // The file is gone: an earlier external conflict / unverifiable
                // notice no longer describes it. A genuine save failure stays.
                self.clear_external_problem(id);
                self.status = Some(
                    "確認後にファイルが見つからなくなったため読み直していません。編集中の内容は保持しています"
                        .to_owned(),
                );
            }
            ReloadVerifyOutcome::Unverifiable { reason } => {
                self.record_save_problem(
                    id,
                    instance,
                    path,
                    DocumentSaveProblem::Failed(format!("{UNVERIFIABLE_PREFIX}{reason}")),
                );
                self.status = Some(
                    "確認後にディスクの内容を確認できなくなったため読み直していません".to_owned(),
                );
            }
        }
        cx.notify();
    }

    fn confirm_overwrite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.sessions.active_id();
        let Some(instance) = self.document_instance(id) else {
            return;
        };
        let session = self.sessions.active();
        if !matches!(
            session.external_status(),
            ExternalStatus::Changed | ExternalStatus::Missing
        ) {
            return;
        }
        // `None` stands for "the file is gone" and is bound just the same.
        let shown = session.observed_fingerprint();
        let revision = session.revision();
        let detail = session.label();
        let receiver = window.prompt(
            gpui::PromptLevel::Warning,
            "ディスク側の変更を、この文書の内容で上書きしますか",
            Some(detail.as_str()),
            &["キャンセル", "上書き"],
            cx,
        );
        cx.spawn(async move |view, cx| {
            let confirmed = receiver.await.ok() == Some(1);
            let _ = view.update(cx, |view, cx| {
                view.finish_overwrite_confirmation(id, instance, revision, shown, confirmed, cx);
            });
        })
        .detach();
    }

    /// Acts on the answer to the overwrite question by queuing a write that
    /// is guarded by the exact version the user was shown.
    fn finish_overwrite_confirmation(
        &mut self,
        id: SessionId,
        instance: DocumentInstance,
        revision: Revision,
        shown: Option<ContentFingerprint>,
        confirmed: bool,
        cx: &mut Context<Self>,
    ) {
        if !confirmed || self.document_instance(id) != Some(instance) {
            return;
        }
        let Some(session) = self.sessions.get_mut(id) else {
            return;
        };
        if session.revision() != revision {
            self.status = Some(
                "確認後に文書が編集されたため上書きしていません。もう一度操作してください"
                    .to_owned(),
            );
            cx.notify();
            return;
        }
        let Some(approval) = session.approve_resolution(ResolutionKind::Overwrite, shown) else {
            self.status = Some(
                "確認後にディスクの内容が更新されました。もう一度、内容を確認してください"
                    .to_owned(),
            );
            cx.notify();
            return;
        };
        self.save_session(id, SaveIntent::Confirmed(approval), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::super::footer::SaveDisplay;
    use super::*;

    fn external_test_root(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-external-change-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn open_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        path: &Path,
    ) -> (gpui::Entity<EditorView>, &'a mut gpui::VisualTestContext) {
        let loaded = OsFileService.load(path).unwrap();
        cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(loaded),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        })
    }

    #[gpui::test]
    fn an_unedited_document_follows_an_external_save(cx: &mut gpui::TestAppContext) {
        let root = external_test_root("follow");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "before\n").unwrap();
        let (view, cx) = open_view(cx, &path);
        cx.run_until_parked();

        std::fs::write(&path, "after the external save\n").unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.editor().document().full_text(),
                "after the external save\n"
            );
            assert!(!view.active_session().is_dirty());
            assert_eq!(view.active_save_display(), SaveDisplay::Saved);
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_reload_stays_in_lines_layout_until_the_parse_publishes_blocks(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = external_test_root("reload-granularity");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "# before\n\nbody\n").unwrap();
        let (view, cx) = open_view(cx, &path);
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.granularity, Granularity::Blocks);
        });

        std::fs::write(&path, "# after\n\nreloaded body\n").unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
        });
        // The read is awaited, but the 40 ms-delayed parse has not run yet.
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.editor().document().full_text(),
                "# after\n\nreloaded body\n"
            );
            assert_eq!(view.granularity, Granularity::Lines);
            assert_eq!(view.sidebar_focus, SidebarFocus::ActiveSession);
        });

        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.granularity, Granularity::Blocks);
            assert_eq!(
                Some(view.heights.len()),
                view.current_index().map(|index| index.len())
            );
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn an_edited_document_keeps_its_text_and_reports_the_conflict(cx: &mut gpui::TestAppContext) {
        let root = external_test_root("conflict");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "before\n").unwrap();
        let (view, cx) = open_view(cx, &path);
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("typed ").unwrap();
            view.after_input(cx);
        });
        let mine = view.read_with(cx, |view, _| view.editor().document().full_text());
        std::fs::write(&path, "changed elsewhere\n").unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), mine);
            assert!(view.active_session().is_dirty());
            assert_eq!(view.active_save_display(), SaveDisplay::Conflict);
            assert!(view.active_resolution_choices().is_some());
        });
        // An ordinary save, repeated, never overwrites the other version.
        for _ in 0..2 {
            view.update(cx, |view, cx| view.save_current(cx));
            cx.run_until_parked();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "changed elsewhere\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_confirmed_overwrite_replaces_only_the_version_that_was_shown(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = external_test_root("overwrite");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "before\n").unwrap();
        let (view, cx) = open_view(cx, &path);
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("typed ").unwrap();
            view.after_input(cx);
        });
        std::fs::write(&path, "changed elsewhere\n").unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
        });
        cx.run_until_parked();

        // The user confirms; then the file changes once more before the
        // write is made. The stale confirmation must not replace it.
        let approval = view.update(cx, |view, _| {
            let shown = view.active_session().observed_fingerprint();
            view.sessions
                .active_mut()
                .approve_resolution(ResolutionKind::Overwrite, shown)
                .expect("the external version is known")
        });
        std::fs::write(&path, "changed again, elsewhere\n").unwrap();
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.save_session(id, SaveIntent::Confirmed(approval), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "changed again, elsewhere\n"
        );
        view.read_with(cx, |view, _| {
            assert!(view.active_session().is_dirty());
            assert!(view.editor().document().full_text().starts_with("typed "));
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_result_for_a_replaced_document_is_dropped(cx: &mut gpui::TestAppContext) {
        let root = external_test_root("stale");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "before\n").unwrap();
        let other = root.join("Other.md");
        std::fs::write(&other, "other\n").unwrap();
        let (view, cx) = open_view(cx, &path);
        cx.run_until_parked();

        std::fs::write(&path, "after\n").unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
            // The document is replaced in place before the read reports.
            let loaded = OsFileService.load(&other).unwrap();
            view.sessions.active_mut().adopt(loaded);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "other\n");
            assert_eq!(view.active_session().path(), Some(other.as_path()));
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Opens a note, records `problem` for it, deletes the file and runs one
    /// check. Returns what the window still holds for that note.
    fn problem_left_after_the_file_goes_missing(
        cx: &mut gpui::TestAppContext,
        label: &str,
        problem: DocumentSaveProblem,
    ) -> Option<DocumentSaveProblem> {
        let root = external_test_root(label);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "before\n").unwrap();
        let (view, cx) = open_view(cx, &path);
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("typed ").unwrap();
            view.after_input(cx);
            let id = view.sessions.active_id();
            let instance = view.document_instance(id).unwrap();
            view.record_save_problem(id, instance, Some(path.clone()), problem);
        });
        std::fs::remove_file(&path).unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
        });
        cx.run_until_parked();

        let left = view.read_with(cx, |view, _| {
            // The missing state, the text and the recovery choice stay.
            assert_eq!(
                view.active_session().external_status(),
                ExternalStatus::Missing
            );
            assert!(view.editor().document().full_text().starts_with("typed "));
            assert!(view.active_resolution_choices().is_some());
            let id = view.sessions.active_id();
            view.save_problems
                .get(&id)
                .map(|scoped| scoped.problem.clone())
        });
        let _ = std::fs::remove_dir_all(&root);
        left
    }

    #[gpui::test]
    fn a_missing_file_clears_an_earlier_external_conflict(cx: &mut gpui::TestAppContext) {
        let left = problem_left_after_the_file_goes_missing(
            cx,
            "missing-conflict",
            DocumentSaveProblem::Conflict,
        );
        assert!(left.is_none(), "stale conflict notice left: {left:?}");
    }

    #[gpui::test]
    fn a_missing_file_clears_an_earlier_unverifiable_notice(cx: &mut gpui::TestAppContext) {
        let left = problem_left_after_the_file_goes_missing(
            cx,
            "missing-unverifiable",
            DocumentSaveProblem::Failed(format!("{UNVERIFIABLE_PREFIX}denied")),
        );
        assert!(left.is_none(), "stale unverifiable notice left: {left:?}");
    }

    #[gpui::test]
    fn the_comparison_blocks_body_input_until_it_is_closed(cx: &mut gpui::TestAppContext) {
        let root = external_test_root("comparison-modal");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "before\n").unwrap();
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = open_view(cx, &path);
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("typed ").unwrap();
            view.after_input(cx);
        });
        std::fs::write(&path, "changed elsewhere\n").unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                window.focus(&view.focus_handle, cx);
                view.open_external_comparison(cx);
            });
        });
        cx.run_until_parked();

        let state = |view: &EditorView| {
            (
                view.editor().document().full_text(),
                view.active_session().is_dirty(),
            )
        };
        let (text, dirty) = view.read_with(cx, |view, _| {
            assert!(view.external_comparison_blocks_body());
            assert!(!view.body_input_capture_enabled());
            state(view)
        });
        assert!(dirty);

        cx.simulate_input("x");
        cx.simulate_keystrokes("backspace enter tab");
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(state(view), (text.clone(), dirty)));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "changed elsewhere\n"
        );

        view.update(cx, |view, cx| view.close_external_comparison(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.external_comparison_blocks_body());
            assert!(view.body_input_capture_enabled());
        });
        cx.simulate_input("x");
        view.read_with(cx, |view, _| {
            assert_ne!(view.editor().document().full_text(), text);
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Records `problem`, confirms a reload, deletes the file before the
    /// verification read and applies its result. Returns what is left.
    fn problem_left_after_the_file_vanishes_during_reload_verification(
        cx: &mut gpui::TestAppContext,
        label: &str,
        problem: DocumentSaveProblem,
    ) -> Option<DocumentSaveProblem> {
        let root = external_test_root(label);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "before\n").unwrap();
        let (view, cx) = open_view(cx, &path);
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("typed ").unwrap();
            view.after_input(cx);
        });
        std::fs::write(&path, "changed elsewhere\n").unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
        });
        cx.run_until_parked();

        let (id, instance, request) = view.update(cx, |view, _| {
            let id = view.sessions.active_id();
            let instance = view.document_instance(id).unwrap();
            let session = view.sessions.active_mut();
            let approval = session
                .approve_resolution(
                    ResolutionKind::DiscardAndReload,
                    session.observed_fingerprint(),
                )
                .unwrap();
            (id, instance, session.begin_confirmed_reload(approval).unwrap())
        });
        view.update(cx, |view, _| {
            view.record_save_problem(id, instance, Some(path.clone()), problem);
        });
        std::fs::remove_file(&path).unwrap();
        let result = run_reload_verification(&OsFileService, &request);
        view.update(cx, |view, cx| {
            view.finish_verified_reload(id, instance, request, result, cx);
        });

        let left = view.read_with(cx, |view, _| {
            assert_eq!(
                view.active_session().external_status(),
                ExternalStatus::Missing
            );
            assert!(view.active_session().is_dirty());
            assert!(view.editor().document().full_text().starts_with("typed "));
            view.save_problems
                .get(&id)
                .map(|scoped| scoped.problem.clone())
        });
        let _ = std::fs::remove_dir_all(&root);
        left
    }

    #[gpui::test]
    fn a_file_missing_at_reload_verification_clears_an_earlier_conflict(
        cx: &mut gpui::TestAppContext,
    ) {
        let left = problem_left_after_the_file_vanishes_during_reload_verification(
            cx,
            "reload-missing-conflict",
            DocumentSaveProblem::Conflict,
        );
        assert!(left.is_none(), "stale conflict notice left: {left:?}");
    }

    #[gpui::test]
    fn a_file_missing_at_reload_verification_keeps_a_genuine_save_failure(
        cx: &mut gpui::TestAppContext,
    ) {
        let left = problem_left_after_the_file_vanishes_during_reload_verification(
            cx,
            "reload-missing-save-failure",
            DocumentSaveProblem::Failed("disk full".to_owned()),
        );
        assert!(matches!(
            left,
            Some(DocumentSaveProblem::Failed(text)) if text == "disk full"
        ));
    }

    #[gpui::test]
    fn a_missing_file_keeps_a_genuine_save_failure(cx: &mut gpui::TestAppContext) {
        let left = problem_left_after_the_file_goes_missing(
            cx,
            "missing-save-failure",
            DocumentSaveProblem::Failed("disk full".to_owned()),
        );
        assert!(matches!(
            left,
            Some(DocumentSaveProblem::Failed(text)) if text == "disk full"
        ));
    }
}
