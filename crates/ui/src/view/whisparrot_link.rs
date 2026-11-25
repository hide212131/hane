//! The editor side of the local link with the whisparrot voice-input app.
//!
//! The link service (`hane-link`) authenticates a connection and hands each
//! request to the UI thread, which executes it here against the window's
//! documents. Everything that reads or changes a document runs in one GPUI turn
//! of [`EditorView::link_execute`]: the target is verified and the edit applied
//! without yielding, so a check can never go stale before the edit it guards.

use super::*;
use hane_link::handles::{HandleTable, RangeRecord};
use hane_link::message::{ErrorCode, HostRequest, LinkError, LinkResult};

/// The application's hook for starting and stopping the link service when the
/// user changes the setting. Implemented by the app, which owns the service.
pub trait WhisparrotLinkController: Send + Sync {
    /// Starts (`true`) or stops (`false`) the service. An error means the
    /// service did not reach the requested state; its message must not contain
    /// secrets.
    fn set_enabled(&self, enabled: bool) -> Result<(), String>;
}

/// Link state that belongs to one window.
#[derive(Default)]
pub(super) struct LinkViewState {
    pub(super) controller: Option<Arc<dyn WhisparrotLinkController>>,
    /// What each inserted utterance is, until it is replaced or expires.
    handles: HandleTable,
    /// The service instance `handles` belong to. A new instance (the link was
    /// turned off and on) starts with none: its clients never saw the old ones.
    instance: Option<String>,
}

/// Where an inserted text is now, or why that cannot be said: the history of
/// the document must reach back to the insertion, no edit may have touched the
/// text since, and exactly the inserted text must still be there.
fn link_resolve_range(
    record: &RangeRecord,
    document: &RopeBuffer,
) -> Result<SourceRange, LinkError> {
    let inserted = Revision(record.inserted_revision);
    let tracked = if document.revision() == inserted {
        record.range
    } else {
        let deltas = document
            .deltas_since(inserted)
            .map_err(|_| LinkError::not_applied(ErrorCode::RangeUnverifiable))?;
        hane_link::track::track_range(record.range, &deltas)
            .map_err(|_| LinkError::not_applied(ErrorCode::RangeChanged))?
    };
    let changed = || LinkError::not_applied(ErrorCode::RangeChanged);
    document.validate_range(tracked).map_err(|_| changed())?;
    let text = document.text(tracked).map_err(|_| changed())?;
    if text != record.text {
        return Err(changed());
    }
    Ok(tracked)
}

fn link_replace_result(
    document_id: &str,
    revision: u64,
    range: SourceRange,
    undo_mode: &str,
) -> serde_json::Value {
    serde_json::json!({
        "documentId": document_id,
        "revision": revision.to_string(),
        "range": { "startByte": range.start.0, "endByte": range.end.0 },
        "undoMode": undo_mode,
    })
}

/// A window the link may address, by the native handle the client captured.
pub struct LinkWindow {
    /// The native window handle (`HWND` on Windows).
    pub hwnd: i64,
    /// This window's id within the service instance, part of every document id.
    pub window_id: u64,
    pub handle: gpui::WindowHandle<EditorView>,
}

/// Runs one queued request on the UI thread and answers it. A request for a
/// window that is not registered (or whose window is gone) is a stale target.
/// The ticket is claimed in the same turn that executes the request, so a
/// deadline that passed while the request waited stops it before it starts.
pub fn run_link_call(
    windows: &[LinkWindow],
    clock: &dyn hane_link::TickClock,
    call: hane_link::HostCall,
    cx: &mut gpui::App,
) {
    let hwnd = match &call.request {
        HostRequest::ContextGet(params) => params.hwnd.0,
        HostRequest::Insert(params) => params.hwnd.0,
        HostRequest::Replace(params) => params.hwnd.0,
    };
    let result = match windows.iter().find(|window| window.hwnd == hwnd) {
        None => Err(LinkError::not_applied(ErrorCode::StaleTarget)),
        Some(window) => {
            let outcome = window.handle.update(cx, |view, window_ref, cx| {
                call.ticket
                    .begin(clock.now_ms(), call.request.deadline_tick())?;
                let context = LinkCall {
                    instance_id: &call.instance_id,
                    window_id: window.window_id,
                    now: Instant::now(),
                };
                view.link_execute(&call.request, &context, window_ref, cx)
            });
            outcome.unwrap_or(Err(LinkError::not_applied(ErrorCode::StaleTarget)))
        }
    };
    let _ = call.reply.send(result);
}

/// What the app tells the view about a call it dispatches.
pub struct LinkCall<'a> {
    /// The service instance the request came in on.
    pub instance_id: &'a str,
    /// This window's registration id within the service instance.
    pub window_id: u64,
    pub now: Instant,
}

impl EditorView {
    /// Installs the hook that starts and stops the link service. Without one
    /// (platforms where the link does not exist) the setting is not offered.
    pub fn attach_whisparrot_link_controller(
        &mut self,
        controller: Option<Arc<dyn WhisparrotLinkController>>,
    ) {
        self.link.controller = controller;
    }

    /// Whether the user allowed the link. Off until they turn it on.
    pub fn whisparrot_link_enabled(&self) -> bool {
        self.settings.whisparrot_link
    }

    /// Whether the settings screen offers the switch at all.
    pub(crate) fn whisparrot_link_available(&self) -> bool {
        self.link.controller.is_some()
    }

    /// Turns the link on or off: starts or stops the service first, and keeps
    /// the stored setting in step with what actually happened. Returns whether
    /// the setting now has the requested value.
    pub(crate) fn set_whisparrot_link(&mut self, enabled: bool, cx: &mut Context<Self>) -> bool {
        if self.settings.whisparrot_link == enabled {
            return true;
        }
        if let Some(controller) = self.link.controller.clone()
            && let Err(message) = controller.set_enabled(enabled)
        {
            // The service did not change state, so neither does the setting.
            self.status = Some(format!(
                "whisparrot との連携を切り替えられませんでした: {message}"
            ));
            cx.notify();
            return false;
        }
        self.settings.whisparrot_link = enabled;
        // The two inputs are chosen explicitly and never run together.
        if enabled && self.settings.voice_composition {
            self.set_voice_composition(false, cx);
        }
        // A failed write keeps the change for this run, like the other settings.
        self.store_settings();
        cx.notify();
        true
    }

    /// Carries out one request from the link on the UI thread.
    pub fn link_execute(
        &mut self,
        request: &HostRequest,
        call: &LinkCall<'_>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> LinkResult {
        self.link_sync_instance(call.instance_id);
        match request {
            HostRequest::ContextGet(params) => self.link_context_get(params, call, window),
            HostRequest::Insert(params) => self.link_insert(params, call, window, cx),
            HostRequest::Replace(params) => self.link_replace(params, call, window, cx),
        }
    }

    /// Forgets every handle of an earlier service instance.
    fn link_sync_instance(&mut self, instance_id: &str) {
        if self.link.instance.as_deref() != Some(instance_id) {
            self.link.handles = HandleTable::default();
            self.link.instance = Some(instance_id.to_owned());
        }
    }

    /// Whether the active document is one the link serves: an unsaved tab, or a
    /// saved `.md` file (any case). Source display modes stay valid.
    fn link_document_supported(&self) -> bool {
        self.sessions.active().path().is_none_or(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        })
    }

    /// Whether the window can serve a request that reads or edits the body.
    ///
    /// With `needs_foreground` the window must be the foreground one with the
    /// body holding the focus (a read at the start of recording, an insertion);
    /// without it (a replacement) it is enough that the selected tab's body can
    /// be changed, wherever the focus is.
    fn link_check_target(&self, window: &Window, needs_foreground: bool) -> Result<(), LinkError> {
        if self.restart_freeze {
            return Err(LinkError::not_applied(ErrorCode::Busy));
        }
        // Reading and comparison displays: the body cannot be changed.
        if self.external_comparison_blocks_body() || !self.body_editable() {
            return Err(LinkError::not_applied(ErrorCode::NotEditable));
        }
        if !self.link_document_supported() {
            return Err(LinkError::not_applied(ErrorCode::UnsupportedDocument));
        }
        // A composition would be committed by an edit: leave it to the user.
        if self.editor().ime().is_some() {
            return Err(LinkError::not_applied(ErrorCode::Busy));
        }
        if needs_foreground {
            if !window.is_window_active() {
                return Err(LinkError::not_applied(ErrorCode::StaleTarget));
            }
            // Settings, a rename, the sidebar filter or an AI popup own the keys.
            if self.settings_open
                || !self.other_input_capture_allows_body()
                || !self.focus_handle.is_focused(window)
            {
                return Err(LinkError::not_applied(ErrorCode::Busy));
            }
        }
        Ok(())
    }

    fn link_document_id(&self, call: &LinkCall<'_>) -> String {
        let session = self.sessions.active();
        hane_link::context::document_id(
            call.instance_id,
            call.window_id,
            session.id().0,
            session.generation(),
        )
    }

    /// `edit.insert`: puts the utterance at the caret as one Undo step, after
    /// checking that the target, version and selection are still the ones the
    /// client read. The checks and the edit happen in this one turn.
    fn link_insert(
        &mut self,
        params: &hane_link::message::InsertParams,
        call: &LinkCall<'_>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> LinkResult {
        self.link_check_target(window, true)?;
        let document_id = self.link_document_id(call);
        if params.document_id != document_id {
            return Err(LinkError::not_applied(ErrorCode::StaleTarget));
        }
        let session = self.sessions.active();
        if params.expected_revision != session.revision().0.to_string() {
            return Err(LinkError::not_applied(ErrorCode::StaleVersion));
        }
        let selection = session.editor().selection();
        if selection.anchor.0 != params.expected_selection.anchor_byte
            || selection.active.0 != params.expected_selection.active_byte
        {
            return Err(LinkError::not_applied(ErrorCode::StaleCursor));
        }
        let (session_id, generation) = (session.id().0, session.generation());
        // Into the empty range at the caret: a selection is not consumed. The
        // caret ends after the inserted text; Undo restores the selection.
        let at = selection.active.0;
        let end = at + params.text.len();
        let recorded = self.editor_mut().replace_range_recorded_with_entry(
            SourceRange::empty(at),
            &params.text,
            Selection::caret(SourceOffset(end)),
        );
        let (summary, entry) = recorded.map_err(|_| LinkError::not_applied(ErrorCode::Internal))?;
        self.status = None;
        self.after_input(cx);
        let text_hash = hane_link::auth::sha256_hex(params.text.as_bytes());
        let range = summary.range_after;
        let handle = self.link.handles.issue(
            RangeRecord::new(
                call.window_id,
                document_id.clone(),
                session_id,
                generation,
                summary.revision_after.0,
                params.text.clone(),
                text_hash.clone(),
                range,
                entry,
            ),
            call.now,
        );
        Ok(serde_json::json!({
            "documentId": document_id,
            "insertedRevision": summary.revision_after.0.to_string(),
            "range": { "startByte": range.start.0, "endByte": range.end.0 },
            "rangeHandle": handle,
            "expectedTextHash": text_hash,
            "expiresInMs": hane_link::message::HANDLE_LIFETIME_MS,
        }))
    }

    /// `edit.replace`: swaps the text an earlier `edit.insert` put in for a
    /// refined version, but only if exactly that text is still where it was put.
    /// The client names the insertion by its handle and never gives coordinates.
    fn link_replace(
        &mut self,
        params: &hane_link::message::ReplaceParams,
        call: &LinkCall<'_>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> LinkResult {
        // The window must only be able to take the edit; it need not be the
        // foreground one, and the focus may be anywhere.
        self.link_check_target(window, false)?;
        let document_id = self.link_document_id(call);
        if params.document_id != document_id {
            return Err(LinkError::not_applied(ErrorCode::StaleTarget));
        }
        let Some(record) = self
            .link
            .handles
            .get(&params.range_handle, call.now)
            .cloned()
        else {
            // Unknown, expired and used-up handles look the same to the client.
            return Err(LinkError::not_applied(ErrorCode::RangeExpired));
        };
        if record.window != call.window_id || record.document_id != document_id {
            return Err(LinkError::not_applied(ErrorCode::StaleTarget));
        }
        if params.inserted_revision != record.inserted_revision.to_string()
            || params.expected_text_hash != record.text_hash
        {
            return Err(LinkError::not_applied(ErrorCode::RangeChanged));
        }
        let session = self.sessions.active();
        let editor = session.editor();
        let range = link_resolve_range(&record, editor.document())?;
        if params.replacement == record.text {
            // Nothing to change: the handle is used up all the same.
            let revision = session.revision().0;
            self.link.handles.consume(&params.range_handle);
            return Ok(link_replace_result(&document_id, revision, range, "none"));
        }
        let selection = editor.selection();
        let (anchor, active) = hane_link::selection::map_selection(
            (selection.anchor.0, selection.active.0),
            range,
            &params.replacement,
        );
        let selection_after = Selection {
            anchor: SourceOffset(anchor),
            active: SourceOffset(active),
        };
        // Right after the insertion, with its Undo step still the newest: fold
        // the replacement into that step, so one Undo removes the utterance.
        // Otherwise it is an Undo step of its own, after whatever came between.
        let amendable = session.revision().0 == record.inserted_revision
            && editor.can_amend_history_entry(
                record.history_entry,
                range.start.0,
                range.len_bytes(),
            );
        let amended = if amendable {
            self.editor_mut()
                .replace_range_amending(
                    record.history_entry,
                    range,
                    &params.replacement,
                    selection_after,
                )
                .map_err(|_| LinkError::not_applied(ErrorCode::Internal))?
        } else {
            None
        };
        let (summary, undo_mode) = match amended {
            Some(summary) => (summary, "merged"),
            None => (
                self.editor_mut()
                    .replace_range_recorded(range, &params.replacement, selection_after)
                    .map_err(|_| LinkError::not_applied(ErrorCode::Internal))?,
                "separate",
            ),
        };
        self.link.handles.consume(&params.range_handle);
        self.status = None;
        self.after_input(cx);
        Ok(link_replace_result(
            &document_id,
            summary.revision_after.0,
            summary.range_after,
            undo_mode,
        ))
    }

    /// `context.get`: the text around the caret, the title and Front Matter of
    /// the target tab, all of one revision of one document.
    fn link_context_get(
        &mut self,
        params: &hane_link::message::ContextGetParams,
        call: &LinkCall<'_>,
        window: &Window,
    ) -> LinkResult {
        self.link_check_target(window, true)?;
        let document_id = self.link_document_id(call);
        if params
            .expected_document_id
            .as_ref()
            .is_some_and(|expected| *expected != document_id)
        {
            return Err(LinkError::not_applied(ErrorCode::StaleTarget));
        }
        let session = self.sessions.active();
        let revision = session.revision().0;
        if params
            .expected_revision
            .as_ref()
            .is_some_and(|expected| *expected != revision.to_string())
        {
            return Err(LinkError::not_applied(ErrorCode::StaleVersion));
        }
        let editor = session.editor();
        let document = editor.document();
        let selection = editor.selection();
        let slices = hane_link::context::slices(
            document,
            selection.active.0,
            params.before_chars(),
            params.after_chars(),
        );
        let front_matter = hane_link::context::front_matter(document);
        let pending: Vec<_> = params
            .pending_range_handles
            .iter()
            .map(|handle| {
                let tracked = self
                    .link
                    .handles
                    .get(handle, call.now)
                    .filter(|record| {
                        record.window == call.window_id && record.document_id == document_id
                    })
                    .and_then(|record| link_resolve_range(record, document).ok());
                match tracked {
                    Some(range) => serde_json::json!({
                        "rangeHandle": handle,
                        "tracked": true,
                        "startByte": range.start.0,
                        "endByte": range.end.0,
                    }),
                    None => serde_json::json!({
                        "rangeHandle": handle,
                        "tracked": false,
                    }),
                }
            })
            .collect();
        let mut result = serde_json::json!({
            "documentId": document_id,
            "revision": revision.to_string(),
            "cursorByte": selection.active.0,
            "selection": {
                "anchorByte": selection.anchor.0,
                "activeByte": selection.active.0,
            },
            "title": hane_link::context::truncate_chars(
                &session.file().short_label(),
                hane_link::context::MAX_TITLE_CHARS,
            ),
            "isUntitled": session.path().is_none(),
            "frontMatter": hane_link::context::front_matter_json(&front_matter),
            "before": slices.before,
            "beforeStartByte": slices.before_start,
            "beforeTruncated": slices.before_truncated,
            "after": slices.after,
            "afterStartByte": slices.after_start,
            "afterTruncated": slices.after_truncated,
            "pendingRanges": pending,
        });
        if params.include_snapshot {
            // A whole-document snapshot is a later step; the ordinary context
            // above is still returned.
            result["snapshotStatus"] = "not_supported".into();
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_link::TickClock;
    use std::sync::Mutex;

    /// Records what the view asked the service to do, and can refuse.
    #[derive(Default)]
    struct FakeController {
        calls: Mutex<Vec<bool>>,
        fail: Mutex<bool>,
    }

    impl WhisparrotLinkController for FakeController {
        fn set_enabled(&self, enabled: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(enabled);
            if *self.fail.lock().unwrap() {
                Err("could not start".to_owned())
            } else {
                Ok(())
            }
        }
    }

    fn open(
        cx: &mut gpui::TestAppContext,
        stores: StateStores,
        controller: Option<Arc<dyn WhisparrotLinkController>>,
    ) -> (gpui::Entity<EditorView>, &mut gpui::VisualTestContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut view = EditorView::from_sessions(
                SessionSet::with_untitled("本文\n", "Untitled"),
                Arc::new(OsFileService),
                stores,
                cx,
            );
            view.attach_whisparrot_link_controller(controller);
            view
        });
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();
        (view, cx)
    }

    /// A real link service on the in-memory transport in front of a real
    /// editor window: requests go through authentication, framing, the ledger
    /// and the queue, then run on the (test) UI thread like in the app.
    struct Rig {
        link: hane_link::testing::TestLink,
        conn: hane_link::testing::MemoryConnection,
        windows: Vec<LinkWindow>,
        next_id: u64,
        instance: String,
    }

    /// The `hwnd` the test window is registered under.
    const HWND: i64 = 4242;

    impl Rig {
        fn start(name: &str, handle: gpui::WindowHandle<EditorView>) -> Self {
            let link = hane_link::testing::TestLink::start(name);
            let mut conn = link.connect();
            let hello = serde_json::json!({"id": 0, "method": "hello", "params": {
                "token": link.token(),
                "protocol": {
                    "min": {"major": 1, "minor": 0},
                    "max": {"major": 1, "minor": 0},
                },
            }});
            conn.send_frame(&serde_json::to_vec(&hello).unwrap())
                .unwrap();
            let reply: serde_json::Value = serde_json::from_slice(
                &conn
                    .receive_frame(Duration::from_secs(3))
                    .unwrap()
                    .expect("hello reply"),
            )
            .unwrap();
            let instance = reply["result"]["instanceId"].as_str().unwrap().to_owned();
            Self {
                link,
                conn,
                windows: vec![LinkWindow {
                    hwnd: HWND,
                    window_id: 1,
                    handle,
                }],
                next_id: 1,
                instance,
            }
        }

        /// Sends a request and runs the UI thread's side until the answer arrives.
        fn request(
            &mut self,
            cx: &mut gpui::VisualTestContext,
            method: &str,
            params: serde_json::Value,
        ) -> serde_json::Value {
            self.next_id += 1;
            let body = serde_json::json!({"id": self.next_id, "method": method, "params": params});
            self.conn
                .send_frame(&serde_json::to_vec(&body).unwrap())
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                while let Ok(call) = self.link.calls.try_recv() {
                    cx.cx
                        .update(|app| run_link_call(&self.windows, &*self.link.clock, call, app));
                }
                match self.conn.receive_frame(Duration::from_millis(5)) {
                    Ok(Some(bytes)) => {
                        let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(reply["id"], serde_json::json!(self.next_id));
                        return reply;
                    }
                    Ok(None) => panic!("the connection closed"),
                    Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(error) => panic!("{error}"),
                }
                assert!(Instant::now() < deadline, "no answer to {method}");
            }
        }

        /// `context.get` with the window's handle filled in.
        fn context(
            &mut self,
            cx: &mut gpui::VisualTestContext,
            mut params: serde_json::Value,
        ) -> serde_json::Value {
            params["hwnd"] = serde_json::json!(HWND);
            self.request(cx, "context.get", params)
        }
    }

    fn error_code(reply: &serde_json::Value) -> String {
        reply["error"]["code"]
            .as_str()
            .unwrap_or_else(|| panic!("not an error: {reply}"))
            .to_owned()
    }

    /// An editor window over `sessions`, focused and ready to be addressed.
    fn open_for_link(
        cx: &mut gpui::TestAppContext,
        sessions: SessionSet,
    ) -> (
        gpui::Entity<EditorView>,
        gpui::WindowHandle<EditorView>,
        &mut gpui::VisualTestContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(sessions, Arc::new(OsFileService), StateStores::memory(), cx)
        });
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();
        cx.update(|window, app| {
            window.activate_window();
            let focus = view.read(app).focus_handle.clone();
            window.focus(&focus, app);
        });
        cx.run_until_parked();
        let handle = cx.update(|window, _| {
            window
                .window_handle()
                .downcast::<EditorView>()
                .expect("an editor window")
        });
        (view, handle, cx)
    }

    fn set_caret(view: &gpui::Entity<EditorView>, cx: &mut gpui::VisualTestContext, at: usize) {
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.editor_mut()
                    .set_selection(Selection::caret(SourceOffset(at)))
                    .unwrap();
            });
        });
    }

    fn markdown_sessions(text: &str, path: &str) -> SessionSet {
        use hane_session::FileIdentity;
        SessionSet::with_loaded(hane_session::LoadedFile {
            document: RopeBuffer::from_text(text),
            identity: FileIdentity::lexical(path),
            stamp: None,
            fingerprint: None,
        })
    }

    #[gpui::test]
    fn context_get_returns_the_text_around_the_caret_of_an_unsaved_tab(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(
            cx,
            SessionSet::with_untitled("# 見出し\n\n本文の一行目。二行目。\n", "Untitled"),
        );
        let mut rig = Rig::start("context-basic", handle);
        let caret = "# 見出し\n\n本文の一行目。".len();
        set_caret(&view, cx, caret);
        let reply = rig.context(cx, serde_json::json!({"beforeChars": 4, "afterChars": 3}));
        let result = &reply["result"];
        assert_eq!(result["before"], "一行目。");
        assert_eq!(result["after"], "二行目");
        assert_eq!(result["cursorByte"], caret);
        assert_eq!(
            result["selection"],
            serde_json::json!({"anchorByte": caret, "activeByte": caret})
        );
        assert_eq!(result["beforeStartByte"], caret - "一行目。".len());
        assert_eq!(result["afterStartByte"], caret);
        assert_eq!(result["beforeTruncated"], true);
        assert_eq!(result["afterTruncated"], true);
        assert_eq!(result["title"], "Untitled");
        assert_eq!(result["isUntitled"], true);
        assert_eq!(
            result["frontMatter"],
            serde_json::json!({"status": "absent"})
        );
        assert!(result["revision"].as_str().unwrap().parse::<u64>().is_ok());
        assert_eq!(result["documentId"].as_str().unwrap().len(), 32);
        assert_eq!(result["pendingRanges"], serde_json::json!([]));
    }

    #[gpui::test]
    fn context_get_reports_a_directional_selection_and_uses_the_active_end_as_the_cursor(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) =
            open_for_link(cx, SessionSet::with_untitled("abcdefghij", "Untitled"));
        let mut rig = Rig::start("context-selection", handle);
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset(7),
                        active: SourceOffset(3),
                    })
                    .unwrap();
            });
        });
        let reply = rig.context(cx, serde_json::json!({"beforeChars": 10, "afterChars": 10}));
        let result = &reply["result"];
        assert_eq!(
            result["selection"],
            serde_json::json!({"anchorByte": 7, "activeByte": 3})
        );
        assert_eq!(result["cursorByte"], 3);
        assert_eq!(result["before"], "abc");
        assert_eq!(result["after"], "defghij");
        assert_eq!(result["beforeTruncated"], false);
        assert_eq!(result["afterTruncated"], false);
    }

    #[gpui::test]
    fn context_get_cuts_by_scalar_values_and_never_inside_a_character(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "🙂🙂🙂a\u{301}🙂|🙂🙂\r\nz";
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled(text, "Untitled"));
        let mut rig = Rig::start("context-unicode", handle);
        let caret = text.find('|').unwrap() + 1;
        set_caret(&view, cx, caret);
        let reply = rig.context(cx, serde_json::json!({"beforeChars": 4, "afterChars": 4}));
        let result = &reply["result"];
        assert_eq!(result["before"], "a\u{301}🙂|");
        assert_eq!(result["after"], "🙂🙂\r\n");
    }

    #[gpui::test]
    fn context_get_defaults_to_1200_before_and_200_after_and_clamps_larger_requests(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = format!("{}|{}", "あ".repeat(2_000), "い".repeat(500));
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled(&text, "Untitled"));
        let mut rig = Rig::start("context-limits", handle);
        set_caret(&view, cx, text.find('|').unwrap() + 1);
        for params in [
            serde_json::json!({}),
            serde_json::json!({"beforeChars": 50_000, "afterChars": 50_000}),
        ] {
            let reply = rig.context(cx, params);
            let result = &reply["result"];
            assert_eq!(result["before"].as_str().unwrap().chars().count(), 1_200);
            assert_eq!(result["after"].as_str().unwrap().chars().count(), 200);
        }
    }

    #[gpui::test]
    fn context_get_returns_front_matter_with_its_status_and_source_range(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "\u{feff}---\ntitle: 設計メモ\ntags: [a]\n---\n本文";
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled(text, "Untitled"));
        let mut rig = Rig::start("context-front", handle);
        set_caret(&view, cx, text.len());
        let reply = rig.context(cx, serde_json::json!({}));
        let front = &reply["result"]["frontMatter"];
        assert_eq!(front["status"], "present");
        assert_eq!(front["text"], "title: 設計メモ\ntags: [a]\n");
        let start = "\u{feff}".len();
        let end = text.find("\n---\n本文").unwrap() + "\n---".len();
        assert_eq!(
            front["range"],
            serde_json::json!({"startByte": start, "endByte": end})
        );
    }

    #[gpui::test]
    fn context_get_marks_an_unclosed_front_matter_unavailable(cx: &mut gpui::TestAppContext) {
        let (_view, handle, cx) = open_for_link(
            cx,
            SessionSet::with_untitled("---\ntitle: x\n本文\n", "Untitled"),
        );
        let mut rig = Rig::start("context-front-open", handle);
        let reply = rig.context(cx, serde_json::json!({}));
        assert_eq!(
            reply["result"]["frontMatter"],
            serde_json::json!({"status": "unavailable"})
        );
    }

    #[gpui::test]
    fn context_get_names_a_saved_markdown_file_and_accepts_any_extension_case(
        cx: &mut gpui::TestAppContext,
    ) {
        for path in ["/notes/メモ.md", "/notes/README.MD"] {
            let (_view, handle, cx) = open_for_link(cx, markdown_sessions("本文", path));
            let mut rig = Rig::start("context-saved", handle);
            let reply = rig.context(cx, serde_json::json!({}));
            let name = path.rsplit('/').next().unwrap();
            assert_eq!(reply["result"]["title"], name, "{path}");
            assert_eq!(reply["result"]["isUntitled"], false);
        }
    }

    #[gpui::test]
    fn context_get_cuts_a_long_title_to_120_characters(cx: &mut gpui::TestAppContext) {
        let name = "長".repeat(150);
        let (_view, handle, cx) =
            open_for_link(cx, markdown_sessions("x", &format!("/notes/{name}.md")));
        let mut rig = Rig::start("context-title", handle);
        let reply = rig.context(cx, serde_json::json!({}));
        assert_eq!(
            reply["result"]["title"].as_str().unwrap().chars().count(),
            120
        );
    }

    #[gpui::test]
    fn a_saved_file_that_is_not_markdown_is_unsupported(cx: &mut gpui::TestAppContext) {
        for path in ["/notes/a.txt", "/notes/noextension", "/notes/a.markdown"] {
            let (_view, handle, cx) = open_for_link(cx, markdown_sessions("x", path));
            let mut rig = Rig::start("context-unsupported", handle);
            let reply = rig.context(cx, serde_json::json!({}));
            assert_eq!(error_code(&reply), "UNSUPPORTED_DOCUMENT", "{path}");
            assert_eq!(reply["error"]["effect"], "not_applied");
        }
    }

    #[gpui::test]
    fn expected_document_and_revision_are_checked(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("context-expected", handle);
        let first = rig.context(cx, serde_json::json!({}));
        let id = first["result"]["documentId"].as_str().unwrap().to_owned();
        let revision = first["result"]["revision"].as_str().unwrap().to_owned();
        let ok = rig.context(
            cx,
            serde_json::json!({"expectedDocumentId": id, "expectedRevision": revision}),
        );
        assert!(ok.get("result").is_some());
        let wrong_doc = rig.context(
            cx,
            serde_json::json!({"expectedDocumentId": "0".repeat(32)}),
        );
        assert_eq!(error_code(&wrong_doc), "STALE_TARGET");
        cx.update(|_, app| view.update(app, |view, _| view.editor_mut().insert_text("x").unwrap()));
        let stale = rig.context(cx, serde_json::json!({"expectedRevision": revision}));
        assert_eq!(error_code(&stale), "STALE_VERSION");
    }

    #[gpui::test]
    fn a_window_that_is_not_registered_is_a_stale_target(cx: &mut gpui::TestAppContext) {
        let (_view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("context-hwnd", handle);
        let reply = rig.request(cx, "context.get", serde_json::json!({"hwnd": HWND + 1}));
        assert_eq!(error_code(&reply), "STALE_TARGET");
    }

    #[gpui::test]
    fn the_document_id_follows_the_tab_not_the_title(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("context-id", handle);
        let before = rig.context(cx, serde_json::json!({}))["result"]["documentId"].clone();
        // Typing does not change which document it is.
        cx.update(|_, app| view.update(app, |view, _| view.editor_mut().insert_text("x").unwrap()));
        let after = rig.context(cx, serde_json::json!({}))["result"]["documentId"].clone();
        assert_eq!(before, after);
        // A second tab is another document, and switching to it changes the target.
        let first = view.read_with(cx, |view, _| view.active_session().id());
        let second = cx.update(|_, app| {
            view.update(app, |view, _| view.sessions.open_untitled("other", "Other"))
        });
        cx.update(|_, app| view.update(app, |view, cx| view.activate_session(second, cx)));
        let other = rig.context(cx, serde_json::json!({}))["result"]["documentId"].clone();
        assert_ne!(before, other);
        cx.update(|_, app| view.update(app, |view, cx| view.activate_session(first, cx)));
        let back = rig.context(cx, serde_json::json!({}))["result"]["documentId"].clone();
        assert_eq!(before, back);
        let _ = &rig.instance;
    }

    #[gpui::test]
    fn context_get_is_refused_while_the_window_cannot_take_the_body(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("context-busy", handle);
        // An update restart freezes the window.
        cx.update(|_, app| view.update(app, |view, _| view.begin_restart_freeze()));
        assert_eq!(error_code(&rig.context(cx, serde_json::json!({}))), "BUSY");
        cx.update(|_, app| view.update(app, |view, _| view.end_restart_freeze()));
        assert!(
            rig.context(cx, serde_json::json!({}))
                .get("result")
                .is_some()
        );
        // The settings screen takes the keys.
        cx.update(|window, app| view.update(app, |view, cx| view.open_settings(window, cx)));
        assert_eq!(error_code(&rig.context(cx, serde_json::json!({}))), "BUSY");
    }

    #[gpui::test]
    fn context_get_is_refused_in_reading_mode_and_while_composing(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("context-readonly", handle);
        // Composition in progress: the user is mid-conversion.
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.editor_mut()
                    .replace_and_mark_text(None, "に", Some(1..1))
                    .unwrap();
            });
        });
        assert_eq!(error_code(&rig.context(cx, serde_json::json!({}))), "BUSY");
        cx.update(|_, app| view.update(app, |view, _| view.editor_mut().commit_composition()));
        assert!(
            rig.context(cx, serde_json::json!({}))
                .get("result")
                .is_some()
        );
        // Reading mode cannot take edits.
        cx.update(|_, app| view.update(app, |view, _| view.editor_mode = EditorMode::Reading));
        assert_eq!(
            error_code(&rig.context(cx, serde_json::json!({}))),
            "NOT_EDITABLE"
        );
    }

    #[gpui::test]
    fn context_get_needs_the_body_focus_and_the_foreground_window(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("context-focus", handle);
        assert!(
            rig.context(cx, serde_json::json!({}))
                .get("result")
                .is_some()
        );
        // Focus moved away from the body (to the sidebar filter's handle).
        cx.update(|window, app| {
            let other = view.read(app).settings_focus_handle.clone();
            window.focus(&other, app);
        });
        cx.run_until_parked();
        assert_eq!(error_code(&rig.context(cx, serde_json::json!({}))), "BUSY");
    }

    #[gpui::test]
    fn include_snapshot_needs_a_version_and_is_not_supported_yet(cx: &mut gpui::TestAppContext) {
        let (_view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("context-snapshot", handle);
        let bare = rig.context(cx, serde_json::json!({"includeSnapshot": true}));
        assert_eq!(error_code(&bare), "INVALID_REQUEST");
        let first = rig.context(cx, serde_json::json!({}));
        let versioned = rig.context(
            cx,
            serde_json::json!({
                "includeSnapshot": true,
                "expectedDocumentId": first["result"]["documentId"],
                "expectedRevision": first["result"]["revision"],
            }),
        );
        assert_eq!(versioned["result"]["snapshotStatus"], "not_supported");
        assert!(versioned["result"].get("markdownSource").is_none());
    }

    /// What a client read with `context.get`, to echo back in an edit.
    struct Read {
        document_id: String,
        revision: String,
        anchor: u64,
        active: u64,
    }

    impl Rig {
        fn read(&mut self, cx: &mut gpui::VisualTestContext) -> Read {
            let reply = self.context(cx, serde_json::json!({}));
            let result = &reply["result"];
            Read {
                document_id: result["documentId"].as_str().unwrap().to_owned(),
                revision: result["revision"].as_str().unwrap().to_owned(),
                anchor: result["selection"]["anchorByte"].as_u64().unwrap(),
                active: result["selection"]["activeByte"].as_u64().unwrap(),
            }
        }

        fn insert_params(&self, read: &Read, operation: &str, text: &str) -> serde_json::Value {
            serde_json::json!({
                "operationId": operation,
                "deadlineTick": (self.link.clock.now_ms() + 60_000).to_string(),
                "hwnd": HWND,
                "documentId": read.document_id,
                "expectedRevision": read.revision,
                "expectedSelection": {"anchorByte": read.anchor, "activeByte": read.active},
                "text": text,
            })
        }

        fn insert(
            &mut self,
            cx: &mut gpui::VisualTestContext,
            read: &Read,
            operation: &str,
            text: &str,
        ) -> serde_json::Value {
            let params = self.insert_params(read, operation, text);
            self.request(cx, "edit.insert", params)
        }
    }

    fn text_of(view: &gpui::Entity<EditorView>, cx: &mut gpui::VisualTestContext) -> String {
        view.read_with(cx, |view, _| view.editor().document().full_text())
    }

    fn selection_of(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
    ) -> (usize, usize) {
        view.read_with(cx, |view, _| {
            let selection = view.editor().selection();
            (selection.anchor.0, selection.active.0)
        })
    }

    fn undo(view: &gpui::Entity<EditorView>, cx: &mut gpui::VisualTestContext) {
        cx.update(|_, app| view.update(app, |view, cx| view.dispatch(EditorCommand::Undo, cx)));
    }

    #[gpui::test]
    fn insert_puts_the_text_at_the_caret_as_one_undo_step_and_returns_a_handle(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("前後", "Untitled"));
        let mut rig = Rig::start("insert-basic", handle);
        set_caret(&view, cx, "前".len());
        let read = rig.read(cx);
        let reply = rig.insert(cx, &read, "op-1", "えーと、こんにちは。");
        let result = &reply["result"];
        assert_eq!(text_of(&view, cx), "前えーと、こんにちは。後");
        let end = "前えーと、こんにちは。".len();
        assert_eq!(
            selection_of(&view, cx),
            (end, end),
            "the caret follows the text"
        );
        assert_eq!(result["documentId"], read.document_id);
        assert_eq!(
            result["range"],
            serde_json::json!({"startByte": "前".len(), "endByte": end})
        );
        assert_eq!(
            result["expectedTextHash"],
            hane_link::auth::sha256_hex("えーと、こんにちは。".as_bytes())
        );
        assert_eq!(result["expiresInMs"], 120_000);
        assert_eq!(result["rangeHandle"].as_str().unwrap().len(), 32);
        let revision = view.read_with(cx, |view, _| view.active_session().revision().0);
        assert_eq!(result["insertedRevision"], revision.to_string());
        assert!(view.read_with(cx, |view, _| view.active_session().is_dirty()));
        // One Undo removes the whole utterance and puts the caret back.
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "前後");
        assert_eq!(selection_of(&view, cx), ("前".len(), "前".len()));
        assert!(!view.read_with(cx, |view, _| view.editor().can_undo()));
    }

    #[gpui::test]
    fn insert_goes_to_the_active_end_of_a_selection_and_does_not_consume_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abcdef", "Untitled"));
        let mut rig = Rig::start("insert-selection", handle);
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset(4),
                        active: SourceOffset(1),
                    })
                    .unwrap();
            });
        });
        let read = rig.read(cx);
        rig.insert(cx, &read, "op-1", "XY");
        assert_eq!(text_of(&view, cx), "aXYbcdef");
        assert_eq!(selection_of(&view, cx), (3, 3));
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "abcdef");
        assert_eq!(
            selection_of(&view, cx),
            (4, 1),
            "Undo restores the selection, with its direction"
        );
    }

    #[gpui::test]
    fn insert_keeps_unicode_crlf_and_combining_characters_as_sent(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("insert-unicode", handle);
        let text = "🙂e\u{301}\r\n🇯🇵\n結合";
        let read = rig.read(cx);
        let reply = rig.insert(cx, &read, "op-1", text);
        assert_eq!(text_of(&view, cx), text);
        assert_eq!(
            reply["result"]["range"],
            serde_json::json!({"startByte": 0, "endByte": text.len()})
        );
        assert_eq!(
            reply["result"]["expectedTextHash"],
            hane_link::auth::sha256_hex(text.as_bytes())
        );
    }

    #[gpui::test]
    fn insert_of_the_largest_allowed_text_works(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("insert-large", handle);
        let text = "あ".repeat(10_000);
        assert!(text.len() <= 32 * 1024);
        let read = rig.read(cx);
        assert!(rig.insert(cx, &read, "op-1", &text).get("result").is_some());
        assert_eq!(text_of(&view, cx).len(), text.len());
    }

    #[gpui::test]
    fn insert_is_refused_if_the_version_the_client_read_has_changed(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("insert-stale-version", handle);
        set_caret(&view, cx, 3);
        let read = rig.read(cx);
        cx.update(|_, app| view.update(app, |view, _| view.editor_mut().insert_text("!").unwrap()));
        let reply = rig.insert(cx, &read, "op-1", "XYZ");
        assert_eq!(error_code(&reply), "STALE_VERSION");
        assert_eq!(reply["error"]["effect"], "not_applied");
        assert_eq!(text_of(&view, cx), "abc!");
    }

    #[gpui::test]
    fn insert_is_refused_if_the_caret_moved_after_the_read(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abcdef", "Untitled"));
        let mut rig = Rig::start("insert-stale-cursor", handle);
        set_caret(&view, cx, 2);
        let read = rig.read(cx);
        set_caret(&view, cx, 5);
        let reply = rig.insert(cx, &read, "op-1", "XYZ");
        assert_eq!(error_code(&reply), "STALE_CURSOR");
        assert_eq!(text_of(&view, cx), "abcdef");
        assert_eq!(
            selection_of(&view, cx),
            (5, 5),
            "the caret is left where the user put it"
        );
        // Extending the selection is a change of selection as well.
        let read = rig.read(cx);
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset(5),
                        active: SourceOffset(6),
                    })
                    .unwrap();
            });
        });
        assert_eq!(
            error_code(&rig.insert(cx, &read, "op-2", "XYZ")),
            "STALE_CURSOR"
        );
    }

    #[gpui::test]
    fn insert_is_refused_for_another_document_or_tab(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("insert-stale-target", handle);
        let read = rig.read(cx);
        let first = view.read_with(cx, |view, _| view.active_session().id());
        let second = cx.update(|_, app| {
            view.update(app, |view, _| view.sessions.open_untitled("other", "Other"))
        });
        cx.update(|_, app| view.update(app, |view, cx| view.activate_session(second, cx)));
        let reply = rig.insert(cx, &read, "op-1", "XYZ");
        assert_eq!(error_code(&reply), "STALE_TARGET");
        assert_eq!(text_of(&view, cx), "other");
        cx.update(|_, app| view.update(app, |view, cx| view.activate_session(first, cx)));
        assert_eq!(text_of(&view, cx), "abc");
    }

    #[gpui::test]
    fn insert_is_refused_in_the_states_that_cannot_take_text_and_leaves_the_document_alone(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("insert-refused", handle);
        let read = rig.read(cx);
        // Composing.
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.editor_mut()
                    .replace_and_mark_text(None, "に", Some(1..1))
                    .unwrap();
            });
        });
        let composing_text = text_of(&view, cx);
        let composing = rig.read_for_error(cx);
        assert_eq!(composing, "BUSY");
        let read_now = Read {
            revision: view.read_with(cx, |view, _| view.active_session().revision().0.to_string()),
            ..read
        };
        let (anchor, active) = selection_of(&view, cx);
        let read_now = Read {
            anchor: anchor as u64,
            active: active as u64,
            ..read_now
        };
        assert_eq!(
            error_code(&rig.insert(cx, &read_now, "op-1", "XYZ")),
            "BUSY"
        );
        assert_eq!(
            text_of(&view, cx),
            composing_text,
            "the composition is untouched"
        );
        assert!(view.read_with(cx, |view, _| view.editor().ime().is_some()));
        cx.update(|_, app| view.update(app, |view, _| view.editor_mut().commit_composition()));
        // Frozen for an update restart.
        let read = rig.read(cx);
        cx.update(|_, app| view.update(app, |view, _| view.begin_restart_freeze()));
        assert_eq!(error_code(&rig.insert(cx, &read, "op-2", "XYZ")), "BUSY");
        cx.update(|_, app| view.update(app, |view, _| view.end_restart_freeze()));
        // Reading mode.
        cx.update(|_, app| view.update(app, |view, _| view.editor_mode = EditorMode::Reading));
        assert_eq!(
            error_code(&rig.insert(cx, &read, "op-3", "XYZ")),
            "NOT_EDITABLE"
        );
        cx.update(|_, app| view.update(app, |view, _| view.editor_mode = EditorMode::Editing));
        // The body lost the focus.
        cx.update(|window, app| {
            let other = view.read(app).settings_focus_handle.clone();
            window.focus(&other, app);
        });
        cx.run_until_parked();
        assert_eq!(error_code(&rig.insert(cx, &read, "op-4", "XYZ")), "BUSY");
        assert_eq!(text_of(&view, cx), composing_text);
    }

    impl Rig {
        /// The error code of a `context.get`, for states that refuse it.
        fn read_for_error(&mut self, cx: &mut gpui::VisualTestContext) -> String {
            error_code(&self.context(cx, serde_json::json!({})))
        }
    }

    #[gpui::test]
    fn insert_is_refused_in_a_saved_file_that_is_not_markdown(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, markdown_sessions("abc", "/notes/a.txt"));
        let mut rig = Rig::start("insert-unsupported", handle);
        let read = Read {
            document_id: "x".repeat(32),
            revision: "0".to_owned(),
            anchor: 0,
            active: 0,
        };
        assert_eq!(
            error_code(&rig.insert(cx, &read, "op-1", "XYZ")),
            "UNSUPPORTED_DOCUMENT"
        );
        assert_eq!(text_of(&view, cx), "abc");
    }

    #[gpui::test]
    fn a_resent_insert_is_answered_from_the_record_and_inserts_once(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("insert-resend", handle);
        let read = rig.read(cx);
        let params = rig.insert_params(&read, "op-1", "一回だけ");
        let first = rig.request(cx, "edit.insert", params.clone());
        let again = rig.request(cx, "edit.insert", params.clone());
        assert_eq!(first["result"], again["result"]);
        assert_eq!(text_of(&view, cx), "一回だけ");
        // Another content under the same id is a conflict and edits nothing.
        let mut other = params;
        other["text"] = serde_json::json!("別の内容");
        let conflict = rig.request(cx, "edit.insert", other);
        assert_eq!(error_code(&conflict), "OPERATION_CONFLICT");
        assert_eq!(text_of(&view, cx), "一回だけ");
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "", "a single Undo step");
    }

    #[gpui::test]
    fn an_insert_whose_deadline_passed_while_it_waited_is_not_applied(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("abc", "Untitled"));
        let mut rig = Rig::start("insert-deadline", handle);
        let read = rig.read(cx);
        let mut params = rig.insert_params(&read, "op-1", "XYZ");
        let deadline = rig.link.clock.now_ms() + 500;
        params["deadlineTick"] = serde_json::json!(deadline.to_string());
        // The clock moves past the deadline before the UI thread gets to it.
        rig.link.clock.set(deadline + 1);
        let reply = rig.request(cx, "edit.insert", params);
        assert_eq!(error_code(&reply), "DEADLINE_EXCEEDED");
        assert_eq!(reply["error"]["effect"], "not_applied");
        assert_eq!(text_of(&view, cx), "abc");
    }

    #[gpui::test]
    fn typing_after_an_insertion_is_its_own_undo_step(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("insert-undo-order", handle);
        let read = rig.read(cx);
        rig.insert(cx, &read, "op-1", "音声");
        cx.update(|_, app| {
            view.update(app, |view, cx| {
                view.insert_text("a", cx);
                view.insert_text("b", cx);
            });
        });
        assert_eq!(text_of(&view, cx), "音声ab");
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "音声");
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "");
    }

    #[gpui::test]
    fn each_insertion_gets_its_own_handle_and_a_new_service_instance_forgets_them(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("insert-handles", handle);
        let read = rig.read(cx);
        let first = rig.insert(cx, &read, "op-1", "一");
        let read = rig.read(cx);
        let second = rig.insert(cx, &read, "op-2", "二");
        assert_ne!(
            first["result"]["rangeHandle"],
            second["result"]["rangeHandle"]
        );
        assert_eq!(view.read_with(cx, |view, _| view.link.handles.len()), 2);
        // Calls from another service instance start from nothing.
        cx.update(|_, app| view.update(app, |view, _| view.link_sync_instance("another-instance")));
        assert_eq!(view.read_with(cx, |view, _| view.link.handles.len()), 0);
    }

    /// What an `edit.insert` answered with, to name the insertion later.
    #[derive(Clone)]
    struct Inserted {
        document_id: String,
        handle: String,
        revision: String,
        hash: String,
        start: usize,
        end: usize,
    }

    impl Rig {
        /// Reads, then inserts `text` at the caret.
        fn insert_here(
            &mut self,
            cx: &mut gpui::VisualTestContext,
            operation: &str,
            text: &str,
        ) -> Inserted {
            let read = self.read(cx);
            let reply = self.insert(cx, &read, operation, text);
            let result = &reply["result"];
            assert!(result.get("rangeHandle").is_some(), "{reply}");
            Inserted {
                document_id: result["documentId"].as_str().unwrap().to_owned(),
                handle: result["rangeHandle"].as_str().unwrap().to_owned(),
                revision: result["insertedRevision"].as_str().unwrap().to_owned(),
                hash: result["expectedTextHash"].as_str().unwrap().to_owned(),
                start: result["range"]["startByte"].as_u64().unwrap() as usize,
                end: result["range"]["endByte"].as_u64().unwrap() as usize,
            }
        }

        fn replace_params(
            &self,
            inserted: &Inserted,
            operation: &str,
            replacement: &str,
        ) -> serde_json::Value {
            serde_json::json!({
                "operationId": operation,
                "deadlineTick": (self.link.clock.now_ms() + 60_000).to_string(),
                "hwnd": HWND,
                "documentId": inserted.document_id,
                "rangeHandle": inserted.handle,
                "insertedRevision": inserted.revision,
                "expectedTextHash": inserted.hash,
                "replacement": replacement,
            })
        }

        fn replace(
            &mut self,
            cx: &mut gpui::VisualTestContext,
            inserted: &Inserted,
            operation: &str,
            replacement: &str,
        ) -> serde_json::Value {
            let params = self.replace_params(inserted, operation, replacement);
            self.request(cx, "edit.replace", params)
        }
    }

    fn type_text(view: &gpui::Entity<EditorView>, cx: &mut gpui::VisualTestContext, text: &str) {
        cx.update(|_, app| view.update(app, |view, cx| view.insert_text(text, cx)));
    }

    fn redo(view: &gpui::Entity<EditorView>, cx: &mut gpui::VisualTestContext) {
        cx.update(|_, app| view.update(app, |view, cx| view.dispatch(EditorCommand::Redo, cx)));
    }

    fn history_depth(view: &gpui::Entity<EditorView>, cx: &mut gpui::VisualTestContext) -> usize {
        // Undo until nothing is left, counting, then redo it all back.
        let mut steps = 0;
        while view.read_with(cx, |view, _| view.editor().can_undo()) {
            undo(view, cx);
            steps += 1;
        }
        for _ in 0..steps {
            redo(view, cx);
        }
        steps
    }

    #[gpui::test]
    fn replace_right_after_the_insertion_merges_into_one_undo_step(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("前後", "Untitled"));
        let mut rig = Rig::start("replace-merged", handle);
        set_caret(&view, cx, "前".len());
        let inserted = rig.insert_here(cx, "op-1", "えーと原文です");
        let reply = rig.replace(cx, &inserted, "op-2", "整えた文です。");
        let result = &reply["result"];
        assert_eq!(result["undoMode"], "merged");
        assert_eq!(text_of(&view, cx), "前整えた文です。後");
        let end = "前整えた文です。".len();
        assert_eq!(
            result["range"],
            serde_json::json!({"startByte": "前".len(), "endByte": end})
        );
        assert_eq!(
            selection_of(&view, cx),
            (end, end),
            "the caret follows the new end"
        );
        let revision = view.read_with(cx, |view, _| view.active_session().revision().0);
        assert_eq!(result["revision"], revision.to_string());
        // One Undo takes the whole utterance away, as if the user had typed it once.
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "前後");
        assert_eq!(selection_of(&view, cx), ("前".len(), "前".len()));
        assert!(!view.read_with(cx, |view, _| view.editor().can_undo()));
        // And Redo brings the refined text back, not the raw one.
        redo(&view, cx);
        assert_eq!(text_of(&view, cx), "前整えた文です。後");
        assert_eq!(selection_of(&view, cx), (end, end));
    }

    #[gpui::test]
    fn a_used_handle_cannot_be_used_again_and_a_stranger_is_expired(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-consumed", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        assert!(
            rig.replace(cx, &inserted, "op-2", "整文")
                .get("result")
                .is_some()
        );
        let again = rig.replace(cx, &inserted, "op-3", "別の整文");
        assert_eq!(error_code(&again), "RANGE_EXPIRED");
        assert_eq!(again["error"]["effect"], "not_applied");
        assert_eq!(text_of(&view, cx), "整文");
        let stranger = Inserted {
            handle: "f".repeat(32),
            ..inserted
        };
        assert_eq!(
            error_code(&rig.replace(cx, &stranger, "op-4", "x")),
            "RANGE_EXPIRED"
        );
    }

    #[gpui::test]
    fn an_expired_handle_is_refused_and_leaves_the_text(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-expired", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        // The same request, 120 seconds later.
        let params = rig.replace_params(&inserted, "op-2", "整文");
        let HostRequest::Replace(params) =
            HostRequest::parse("edit.replace", params).unwrap().unwrap()
        else {
            panic!("a replacement");
        };
        let later = LinkCall {
            instance_id: &rig.instance,
            window_id: 1,
            now: Instant::now() + Duration::from_secs(121),
        };
        let reply = cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.link_execute(&HostRequest::Replace(params.clone()), &later, window, cx)
            })
        });
        assert_eq!(reply, Err(LinkError::not_applied(ErrorCode::RangeExpired)));
        assert_eq!(text_of(&view, cx), "原文");
    }

    #[gpui::test]
    fn typing_after_the_insertion_leaves_the_range_alone_and_the_replacement_is_its_own_undo_step(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-after", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        type_text(&view, cx, "ab");
        let reply = rig.replace(cx, &inserted, "op-2", "整文です");
        assert_eq!(reply["result"]["undoMode"], "separate");
        assert_eq!(text_of(&view, cx), "整文ですab");
        let end = "整文です".len() + 2;
        assert_eq!(
            selection_of(&view, cx),
            (end, end),
            "the caret keeps its place after the text"
        );
        // Three independent steps, newest first: the replacement, the typing, the insertion.
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "原文ab");
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "原文");
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "");
    }

    #[gpui::test]
    fn typing_at_the_start_of_the_range_moves_it_and_the_replacement_still_applies(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-start", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        set_caret(&view, cx, inserted.start);
        type_text(&view, cx, ">>");
        let reply = rig.replace(cx, &inserted, "op-2", "整文");
        assert_eq!(reply["result"]["undoMode"], "separate");
        assert_eq!(text_of(&view, cx), ">>整文");
        assert_eq!(
            reply["result"]["range"],
            serde_json::json!({"startByte": 2, "endByte": 2 + "整文".len()})
        );
    }

    #[gpui::test]
    fn an_edit_before_the_range_shifts_it(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("本文", "Untitled"));
        let mut rig = Rig::start("replace-shift", handle);
        set_caret(&view, cx, "本文".len());
        let inserted = rig.insert_here(cx, "op-1", "🙂原文");
        set_caret(&view, cx, 0);
        type_text(&view, cx, "冒頭\r\n");
        assert!(
            rig.replace(cx, &inserted, "op-2", "🙂整文")
                .get("result")
                .is_some()
        );
        assert_eq!(text_of(&view, cx), "冒頭\r\n本文🙂整文");
    }

    #[gpui::test]
    fn an_edit_inside_the_range_is_refused_even_if_the_text_comes_back(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-inside", handle);
        let inserted = rig.insert_here(cx, "op-1", "あいう");
        set_caret(&view, cx, "あ".len());
        type_text(&view, cx, "x");
        let reply = rig.replace(cx, &inserted, "op-2", "整文");
        assert_eq!(error_code(&reply), "RANGE_CHANGED");
        assert_eq!(text_of(&view, cx), "あxいう");
        // Deleting the stray character restores the text, but the edit was made.
        cx.update(|_, app| {
            view.update(app, |view, cx| view.dispatch(EditorCommand::Backspace, cx))
        });
        assert_eq!(text_of(&view, cx), "あいう");
        let reply = rig.replace(cx, &inserted, "op-3", "整文");
        assert_eq!(error_code(&reply), "RANGE_CHANGED");
        assert_eq!(reply["error"]["effect"], "not_applied");
        assert_eq!(text_of(&view, cx), "あいう");
    }

    #[gpui::test]
    fn deleting_or_replacing_over_the_range_is_refused(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-overlap", handle);
        let inserted = rig.insert_here(cx, "op-1", "あいう");
        // Select the tail of the range and the text after it, then type over it.
        type_text(&view, cx, "後");
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset("あ".len() * 2),
                        active: SourceOffset("あいう後".len()),
                    })
                    .unwrap();
            });
        });
        type_text(&view, cx, "Z");
        assert_eq!(text_of(&view, cx), "あいZ");
        assert_eq!(
            error_code(&rig.replace(cx, &inserted, "op-2", "整文")),
            "RANGE_CHANGED"
        );
        assert_eq!(text_of(&view, cx), "あいZ");
    }

    #[gpui::test]
    fn undoing_the_insertion_makes_the_handle_useless_even_after_redo(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-undo", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        undo(&view, cx);
        assert_eq!(
            error_code(&rig.replace(cx, &inserted, "op-2", "整文")),
            "RANGE_CHANGED"
        );
        redo(&view, cx);
        assert_eq!(text_of(&view, cx), "原文");
        assert_eq!(
            error_code(&rig.replace(cx, &inserted, "op-3", "整文")),
            "RANGE_CHANGED"
        );
        assert_eq!(text_of(&view, cx), "原文");
    }

    #[gpui::test]
    fn a_mismatching_hash_or_revision_is_refused(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-expectations", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        let wrong_hash = Inserted {
            hash: hane_link::auth::sha256_hex(b"other"),
            ..inserted.clone()
        };
        assert_eq!(
            error_code(&rig.replace(cx, &wrong_hash, "op-2", "x")),
            "RANGE_CHANGED"
        );
        let wrong_revision = Inserted {
            revision: "999999".to_owned(),
            ..inserted.clone()
        };
        assert_eq!(
            error_code(&rig.replace(cx, &wrong_revision, "op-3", "x")),
            "RANGE_CHANGED"
        );
        assert_eq!(text_of(&view, cx), "原文");
        // The handle was not used up by the refusals.
        assert!(
            rig.replace(cx, &inserted, "op-4", "整文")
                .get("result")
                .is_some()
        );
    }

    #[gpui::test]
    fn a_replacement_equal_to_the_inserted_text_changes_nothing_and_uses_the_handle_up(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-same", handle);
        let inserted = rig.insert_here(cx, "op-1", "同じ文");
        let revision = view.read_with(cx, |view, _| view.active_session().revision().0);
        let reply = rig.replace(cx, &inserted, "op-2", "同じ文");
        assert_eq!(reply["result"]["undoMode"], "none", "{reply}");
        assert_eq!(
            reply["result"]["range"],
            serde_json::json!({"startByte": 0, "endByte": "同じ文".len()})
        );
        assert_eq!(reply["result"]["revision"], revision.to_string());
        assert_eq!(text_of(&view, cx), "同じ文");
        // No revision, no Undo step: the one Undo still removes the insertion.
        assert_eq!(
            view.read_with(cx, |view, _| view.active_session().revision().0),
            revision
        );
        assert_eq!(history_depth(&view, cx), 1);
        assert_eq!(
            error_code(&rig.replace(cx, &inserted, "op-3", "別")),
            "RANGE_EXPIRED"
        );
    }

    #[gpui::test]
    fn a_replacement_is_refused_for_another_tab_and_after_the_document_was_replaced(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-target", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        let first = view.read_with(cx, |view, _| view.active_session().id());
        let second = cx.update(|_, app| {
            view.update(app, |view, _| view.sessions.open_untitled("other", "Other"))
        });
        cx.update(|_, app| view.update(app, |view, cx| view.activate_session(second, cx)));
        assert_eq!(
            error_code(&rig.replace(cx, &inserted, "op-2", "整文")),
            "STALE_TARGET"
        );
        assert_eq!(text_of(&view, cx), "other");
        cx.update(|_, app| view.update(app, |view, cx| view.activate_session(first, cx)));
        // The tab's document is replaced (an external reload adopts new content).
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.sessions.active_mut().adopt(hane_session::LoadedFile {
                    document: RopeBuffer::from_text("原文"),
                    identity: hane_session::FileIdentity::lexical("/notes/a.md"),
                    stamp: None,
                    fingerprint: None,
                });
            })
        });
        assert_eq!(
            error_code(&rig.replace(cx, &inserted, "op-3", "整文")),
            "STALE_TARGET"
        );
        assert_eq!(text_of(&view, cx), "原文");
    }

    #[gpui::test]
    fn a_replacement_does_not_need_the_foreground_but_does_not_commit_a_composition(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-focus", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        // The user is working elsewhere in the window: the focus left the body.
        cx.update(|window, app| {
            let other = view.read(app).settings_focus_handle.clone();
            window.focus(&other, app);
        });
        cx.run_until_parked();
        let selection_before = selection_of(&view, cx);
        // A pending composition blocks it before anything changes.
        cx.update(|window, app| {
            let focus = view.read(app).focus_handle.clone();
            window.focus(&focus, app);
        });
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.editor_mut()
                    .replace_and_mark_text(None, "に", Some(1..1))
                    .unwrap();
            });
        });
        let composing = text_of(&view, cx);
        let refused = rig.replace(cx, &inserted, "op-2", "整文");
        assert_eq!(error_code(&refused), "BUSY");
        assert_eq!(text_of(&view, cx), composing);
        assert!(view.read_with(cx, |view, _| view.editor().ime().is_some()));
        cx.update(|_, app| view.update(app, |view, _| view.editor_mut().commit_composition()));
        cx.update(|window, app| {
            let other = view.read(app).settings_focus_handle.clone();
            window.focus(&other, app);
        });
        cx.run_until_parked();
        let reply = rig.replace(cx, &inserted, "op-3", "整文");
        // The composition was committed by the user above, so the range now has
        // text typed after it: still outside, and still a valid replacement.
        assert!(reply.get("result").is_some(), "{reply}");
        let _ = selection_before;
    }

    #[gpui::test]
    fn a_replacement_in_reading_mode_is_refused(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-reading", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        cx.update(|_, app| view.update(app, |view, _| view.editor_mode = EditorMode::Reading));
        assert_eq!(
            error_code(&rig.replace(cx, &inserted, "op-2", "整文")),
            "NOT_EDITABLE"
        );
        cx.update(|_, app| view.update(app, |view, _| view.editor_mode = EditorMode::Editing));
        assert_eq!(text_of(&view, cx), "原文");
    }

    #[gpui::test]
    fn a_caret_inside_the_range_keeps_its_distance_and_is_clamped_to_the_new_end(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-caret", handle);
        let inserted = rig.insert_here(cx, "op-1", "あいうえお");
        // The user clicked between 'う' and 'え'.
        set_caret(&view, cx, "あいう".len());
        assert!(
            rig.replace(cx, &inserted, "op-2", "かきくけこさし")
                .get("result")
                .is_some()
        );
        assert_eq!(selection_of(&view, cx), ("かきく".len(), "かきく".len()));
        // Another utterance, caret near its end, replaced by something shorter.
        let inserted = rig.insert_here(cx, "op-3", "さしすせそ");
        let start = inserted.start;
        set_caret(&view, cx, start + "さしすせ".len());
        assert!(
            rig.replace(cx, &inserted, "op-4", "ab")
                .get("result")
                .is_some()
        );
        assert_eq!(
            selection_of(&view, cx),
            (start + "ab".len(), start + "ab".len()),
            "past the new end goes to the end"
        );
    }

    #[gpui::test]
    fn a_selection_through_the_range_keeps_its_outer_end_and_direction(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("頭", "Untitled"));
        let mut rig = Rig::start("replace-selection", handle);
        set_caret(&view, cx, "頭".len());
        let inserted = rig.insert_here(cx, "op-1", "原文");
        type_text(&view, cx, "末");
        cx.update(|_, app| {
            view.update(app, |view, _| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset("頭原文末".len()),
                        active: SourceOffset(0),
                    })
                    .unwrap();
            });
        });
        assert!(
            rig.replace(cx, &inserted, "op-2", "整えた文")
                .get("result")
                .is_some()
        );
        assert_eq!(text_of(&view, cx), "頭整えた文末");
        assert_eq!(selection_of(&view, cx), ("頭整えた文末".len(), 0));
    }

    #[gpui::test]
    fn a_history_that_no_longer_reaches_the_insertion_is_unverifiable(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("replace-unverifiable", handle);
        let inserted = rig.insert_here(cx, "op-1", "原文");
        // More edits after the range than the document remembers.
        cx.update(|_, app| {
            view.update(app, |view, _| {
                for _ in 0..4_200 {
                    view.editor_mut().insert_text("x").unwrap();
                }
            })
        });
        let reply = rig.replace(cx, &inserted, "op-2", "整文");
        assert_eq!(error_code(&reply), "RANGE_UNVERIFIABLE");
        assert_eq!(reply["error"]["effect"], "not_applied");
        assert!(text_of(&view, cx).starts_with("原文x"));
    }

    #[gpui::test]
    fn two_pending_utterances_are_replaced_independently_in_either_order(
        cx: &mut gpui::TestAppContext,
    ) {
        for newest_first in [false, true] {
            let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
            let mut rig = Rig::start("replace-two", handle);
            let first = rig.insert_here(cx, "op-1", "一つ目の原文");
            let second = rig.insert_here(cx, "op-2", "二つ目の原文");
            assert_eq!(text_of(&view, cx), "一つ目の原文二つ目の原文");
            let order = if newest_first {
                [
                    (&second, "二つ目の整文", "op-3"),
                    (&first, "一つ目の整文", "op-4"),
                ]
            } else {
                [
                    (&first, "一つ目の整文", "op-3"),
                    (&second, "二つ目の整文", "op-4"),
                ]
            };
            let mut modes = Vec::new();
            for (inserted, replacement, operation) in order {
                let reply = rig.replace(cx, inserted, operation, replacement);
                assert!(reply.get("result").is_some(), "{reply}");
                modes.push(reply["result"]["undoMode"].as_str().unwrap().to_owned());
            }
            assert_eq!(text_of(&view, cx), "一つ目の整文二つ目の整文");
            // Only an insertion that is still the newest Undo step can merge.
            assert_eq!(
                modes,
                if newest_first {
                    ["merged", "separate"]
                } else {
                    ["separate", "separate"]
                }
            );
        }
    }

    #[gpui::test]
    fn context_get_reports_where_pending_insertions_are_now(cx: &mut gpui::TestAppContext) {
        let (view, handle, cx) = open_for_link(cx, SessionSet::with_untitled("", "Untitled"));
        let mut rig = Rig::start("pending-ranges", handle);
        let first = rig.insert_here(cx, "op-1", "一つ目");
        let second = rig.insert_here(cx, "op-2", "二つ目");
        // Text typed in front moves both ranges.
        set_caret(&view, cx, 0);
        type_text(&view, cx, "前");
        let reply = rig.context(
            cx,
            serde_json::json!({"pendingRangeHandles": [first.handle, second.handle]}),
        );
        let pending = reply["result"]["pendingRanges"].as_array().unwrap().clone();
        let shift = "前".len();
        assert_eq!(pending[0]["rangeHandle"], first.handle);
        assert_eq!(pending[0]["tracked"], true);
        assert_eq!(pending[0]["startByte"], first.start + shift);
        assert_eq!(pending[0]["endByte"], first.end + shift);
        assert_eq!(pending[1]["startByte"], second.start + shift);
        // An edit inside one of them, and a handle nobody issued.
        set_caret(&view, cx, shift + "一".len());
        type_text(&view, cx, "!");
        let reply = rig.context(
            cx,
            serde_json::json!({"pendingRangeHandles": [first.handle, second.handle]}),
        );
        let pending = reply["result"]["pendingRanges"].as_array().unwrap().clone();
        assert_eq!(pending[0]["tracked"], false);
        assert_eq!(pending[1]["tracked"], true);
        let reply = rig.context(
            cx,
            serde_json::json!({"pendingRangeHandles": ["0".repeat(32)]}),
        );
        assert_eq!(reply["result"]["pendingRanges"][0]["tracked"], false);
    }

    #[gpui::test]
    fn the_link_is_off_by_default_and_the_switch_starts_and_stops_the_service(
        cx: &mut gpui::TestAppContext,
    ) {
        let stores = StateStores::memory();
        let controller = Arc::new(FakeController::default());
        let (view, cx) = open(cx, stores.clone(), Some(controller.clone()));
        view.read_with(cx, |view, _| assert!(!view.whisparrot_link_enabled()));
        assert!(controller.calls.lock().unwrap().is_empty());

        assert!(view.update(cx, |view, cx| view.set_whisparrot_link(true, cx)));
        assert!(stores.settings().load().whisparrot_link, "persisted");
        // Switching to the value it already has does not touch the service.
        assert!(view.update(cx, |view, cx| view.set_whisparrot_link(true, cx)));
        assert!(view.update(cx, |view, cx| view.set_whisparrot_link(false, cx)));
        assert!(!stores.settings().load().whisparrot_link);
        assert_eq!(*controller.calls.lock().unwrap(), vec![true, false]);
    }

    #[gpui::test]
    fn a_service_that_cannot_start_leaves_the_setting_off(cx: &mut gpui::TestAppContext) {
        let stores = StateStores::memory();
        let controller = Arc::new(FakeController::default());
        *controller.fail.lock().unwrap() = true;
        let (view, cx) = open(cx, stores.clone(), Some(controller));
        assert!(!view.update(cx, |view, cx| view.set_whisparrot_link(true, cx)));
        view.read_with(cx, |view, _| {
            assert!(!view.whisparrot_link_enabled());
            assert!(
                view.status
                    .as_deref()
                    .is_some_and(|status| status.contains("連携"))
            );
        });
        assert!(!stores.settings().load().whisparrot_link);
    }

    #[gpui::test]
    fn the_settings_screen_offers_the_switch_only_where_the_link_exists(
        cx: &mut gpui::TestAppContext,
    ) {
        for (controller, expected) in [
            (
                Some(Arc::new(FakeController::default()) as Arc<dyn WhisparrotLinkController>),
                true,
            ),
            (None, false),
        ] {
            let (view, cx) = open(cx, StateStores::memory(), controller);
            cx.update(|window, app| view.update(app, |view, cx| view.open_settings(window, cx)));
            cx.run_until_parked();
            assert_eq!(
                cx.debug_bounds("settings-whisparrot-link-card").is_some(),
                expected
            );
        }
    }

    #[gpui::test]
    fn clicking_the_checkbox_flips_the_setting(cx: &mut gpui::TestAppContext) {
        let stores = StateStores::memory();
        let controller = Arc::new(FakeController::default());
        let (view, cx) = open(cx, stores.clone(), Some(controller.clone()));
        cx.update(|window, app| view.update(app, |view, cx| view.open_settings(window, cx)));
        cx.run_until_parked();
        let card = cx
            .debug_bounds("settings-whisparrot-link-card")
            .expect("the link card is laid out");
        cx.simulate_click(
            gpui::point(card.left() + px(25.0), card.top() + px(24.0)),
            gpui::Modifiers::none(),
        );
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.whisparrot_link_enabled()));
        assert!(stores.settings().load().whisparrot_link);
        assert_eq!(*controller.calls.lock().unwrap(), vec![true]);
    }
}
