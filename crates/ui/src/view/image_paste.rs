//! Clipboard image paste (Issue #591), the GPUI side.
//!
//! One body Paste reads the OS clipboard **once** into a [`ClipboardSnapshot`]
//! and this module's dispatcher chooses between its image and its text, so no
//! caller re-reads the clipboard or re-implements the priority. A real raster
//! image wins for the document body; everything else (search, settings, rename
//! fields, a clipboard without an image) keeps the text behavior.
//!
//! An image paste is a background job. At request time it captures the session,
//! document instance (including the workspace epoch), revision, selection, the
//! document's base directory, the draft identity and a job number; when the
//! PNG is saved it re-checks all of them on the UI thread and only then makes
//! one recorded replacement. Anything that moved aborts the paste and removes
//! the file this job created.

use super::*;
use crate::clipboard_image::{
    self as clip, ImagePasteError, InsertionShape, LineContext, NextLine, PreviousLine,
};
use crate::view::input_routing::InputTarget;
use hane_markdown::NodeKind;

const NOT_PARAGRAPH: &str =
    "この位置には画像を貼り付けられません。通常の段落に移動してから貼り付けてください";

/// A decodable raster image taken from the clipboard.
#[derive(Debug)]
pub(crate) struct ClipboardRaster {
    format: image::ImageFormat,
    bytes: Vec<u8>,
}

/// Everything one Paste needs from the OS clipboard, read in a single call so
/// the text and the image always come from the same clipboard generation.
#[derive(Debug, Default)]
pub(crate) struct ClipboardSnapshot {
    text: Option<String>,
    raster: Option<Arc<ClipboardRaster>>,
}

impl ClipboardSnapshot {
    pub(crate) fn from_item(item: &ClipboardItem) -> Self {
        let raster = item.entries().iter().find_map(|entry| match entry {
            gpui::ClipboardEntry::Image(source) => {
                clip::source_format(source.format).map(|format| {
                    Arc::new(ClipboardRaster {
                        format,
                        bytes: source.bytes.clone(),
                    })
                })
            }
            _ => None,
        });
        Self {
            text: item.text(),
            raster,
        }
    }

    pub(crate) fn read(cx: &gpui::App) -> Self {
        cx.read_from_clipboard()
            .map(|item| Self::from_item(&item))
            .unwrap_or_default()
    }

    pub(crate) fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Whether a body Paste would do something with this snapshot.
    pub(crate) fn has_paste_content(&self) -> bool {
        self.raster.is_some() || self.text.as_deref().is_some_and(|text| !text.is_empty())
    }
}

/// What a still-running or waiting image paste needs to be judged again.
#[derive(Clone, Debug)]
struct ImagePasteTicket {
    id: SessionId,
    instance: DocumentInstance,
    revision: Revision,
    selection: Selection,
    base: PathBuf,
    draft: Option<DraftId>,
    job: u64,
}

/// A paste that is waiting for the user to choose where an untitled document
/// is saved. Dropped by any answer other than a completed save.
struct PendingImagePaste {
    id: SessionId,
    instance: DocumentInstance,
    revision: Revision,
    selection: Selection,
    raster: Arc<ClipboardRaster>,
}

#[derive(Default)]
pub(super) struct ImagePasteState {
    /// Job number of the newest paste request. A result (and the status text
    /// it may clear) only counts for the job that is still the newest, so an
    /// older job can neither overwrite nor clear a newer job's message.
    latest_job: u64,
    pending_save: Option<PendingImagePaste>,
}

/// The saved file and size of a finished background job.
struct PastedImage {
    saved: clip::SavedImage,
    width: u32,
    height: u32,
}

/// A Save As that has to copy generated images first.
struct SaveAsCopyTicket {
    id: SessionId,
    instance: DocumentInstance,
    revision: Revision,
    path: PathBuf,
    close_after_save: bool,
    from: PathBuf,
}

impl EditorView {
    /// The directory a document's relative resources resolve against: its own
    /// file's directory, or — for an unnamed note in a work folder — the
    /// folder its draft will be created in. Never the process working
    /// directory. This is the one place that connects the draft target.
    pub(super) fn document_base_directory(&self, id: SessionId) -> Option<PathBuf> {
        let session = self.sessions.get(id)?;
        session
            .file()
            .directory()
            .map(Path::to_path_buf)
            .or_else(|| {
                self.work_folder_drafts
                    .get(&id)
                    .map(|draft| draft.target_directory.clone())
            })
    }

    /// The resolver the renderer gives the active document's images.
    pub(super) fn document_resource_resolver(&self) -> hane_session::ResourceResolver {
        hane_session::ResourceResolver::for_directory(
            self.document_base_directory(self.sessions.active_id())
                .as_deref(),
        )
    }

    /// The dispatcher of a body Paste: a real image first, otherwise the text.
    /// An image that cannot be saved never falls back to its text companion.
    pub(crate) fn paste_into_body(&mut self, snapshot: ClipboardSnapshot, cx: &mut Context<Self>) {
        if let Some(raster) = snapshot.raster {
            self.paste_clipboard_image(raster, cx);
        } else if let Some(text) = snapshot.text {
            self.dispatch(EditorCommand::Insert(&text), cx);
        }
    }

    fn paste_clipboard_image(&mut self, raster: Arc<ClipboardRaster>, cx: &mut Context<Self>) {
        // A read-only body must not even start the file creation.
        if !self.body_editable() || self.external_comparison_blocks_body() {
            return;
        }
        if self.editor().ime().is_some() {
            self.status = Some("変換を確定してから画像を貼り付けてください".to_owned());
            cx.notify();
            return;
        }
        let id = self.sessions.active_id();
        let Some(instance) = self.document_instance(id) else {
            return;
        };
        let revision = self.sessions.active().revision();
        let selection = self.editor().selection();
        let shape = match self.plan_image_line(selection) {
            Ok(shape) => shape,
            Err(message) => {
                self.status = Some(message);
                cx.notify();
                return;
            }
        };
        let Some(base) = self.document_base_directory(id) else {
            // A standalone untitled document has no directory next to which
            // `images` could live: ask where to save it, nothing is created
            // before the answer, and the paste resumes only after the save.
            self.image_paste.pending_save = Some(PendingImagePaste {
                id,
                instance,
                revision,
                selection,
                raster,
            });
            self.status =
                Some("画像を保存する場所を決めるため、文書の保存先を選んでください".to_owned());
            self.prompt_save_as_for(id, cx);
            cx.notify();
            return;
        };
        self.image_paste.latest_job += 1;
        let ticket = ImagePasteTicket {
            id,
            instance,
            revision,
            selection,
            base,
            draft: self.work_folder_drafts.get(&id).map(|draft| draft.draft_id),
            job: self.image_paste.latest_job,
        };
        self.status = Some("画像を保存しています…".to_owned());
        let base = ticket.base.clone();
        cx.spawn(async move |view, cx| {
            // Decoding, encoding and the file I/O stay off the UI thread.
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let encoded = clip::encode_clipboard_image(&raster.bytes, raster.format)?;
                    let saved = clip::save_png(&base, &encoded.bytes)?;
                    Ok::<_, ImagePasteError>(PastedImage {
                        saved,
                        width: encoded.width,
                        height: encoded.height,
                    })
                })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_image_paste(ticket, shape, outcome, cx));
        })
        .detach();
        cx.notify();
    }

    /// Decides whether `selection` is somewhere a stand-alone image line is
    /// safe — an ordinary paragraph, or an empty document — and how many line
    /// breaks that line needs. Only the blocks under the selection and the two
    /// neighbouring lines are read; the document is not re-parsed.
    fn plan_image_line(&self, selection: Selection) -> Result<InsertionShape, String> {
        let range = selection.range();
        let document = self.editor().document();
        let length = document.len_bytes().0;
        if length > 0 {
            let Some(index) = self.current_index() else {
                return Err("文書の解析が終わってから、もう一度貼り付けてください".to_owned());
            };
            let ordinal_for = |offset: SourceOffset| {
                index.ordinal_at(offset).or_else(|| {
                    if offset.0 >= length {
                        index.len().checked_sub(1)
                    } else {
                        None
                    }
                })
            };
            let (Some(first), Some(last)) = (ordinal_for(range.start), ordinal_for(range.end))
            else {
                return Err(NOT_PARAGRAPH.to_owned());
            };
            for ordinal in first..=last {
                match index.block(ordinal) {
                    Some(block) if block.kind == NodeKind::Paragraph => {}
                    _ => return Err(NOT_PARAGRAPH.to_owned()),
                }
            }
        }
        let text_error = |error: BufferError| format!("画像の貼り付け位置を確認できません: {error}");
        let start_line = document.line_for_offset(range.start).map_err(text_error)?;
        let end_line = document.line_for_offset(range.end).map_err(text_error)?;
        let start_full = document.line_range(start_line).map_err(text_error)?;
        let end_full = document.line_range(end_line).map_err(text_error)?;
        let end_content = document.line_content_range(end_line).map_err(text_error)?;
        let prefix = document
            .text(SourceRange::new(start_full.start.0, range.start.0))
            .map_err(text_error)?;
        let suffix = if range.end.0 <= end_content.end.0 {
            document
                .text(SourceRange::new(range.end.0, end_content.end.0))
                .map_err(text_error)?
        } else {
            String::new()
        };
        let line_blank = |line: LineId| -> bool {
            document
                .line_content_range(line)
                .and_then(|content| document.text(content))
                .is_ok_and(|text| text.trim().is_empty())
        };
        let previous = if start_line.0 == 0 {
            PreviousLine::DocumentStart
        } else if line_blank(LineId(start_line.0 - 1)) {
            PreviousLine::Blank
        } else {
            PreviousLine::Text
        };
        let next = if end_full.end.0 == end_content.end.0 {
            NextLine::DocumentEnd
        } else if end_line.0 + 1 >= document.line_count() || line_blank(LineId(end_line.0 + 1)) {
            NextLine::Blank
        } else {
            NextLine::Text
        };
        let line_ending = [start_line, end_line]
            .into_iter()
            .chain(start_line.0.checked_sub(1).map(LineId))
            .find_map(|line| {
                let full = document.line_range(line).ok()?;
                let content = document.line_content_range(line).ok()?;
                (full.end.0 > content.end.0)
                    .then(|| document.text(SourceRange::new(content.end.0, full.end.0)).ok())
                    .flatten()
            })
            .unwrap_or_else(|| "\n".to_owned());
        Ok(clip::plan_shape(LineContext {
            prefix: &prefix,
            suffix: &suffix,
            previous,
            next,
            line_ending: &line_ending,
        }))
    }

    /// Why a finished paste may no longer be applied, or `Ok` when every
    /// captured fact still holds.
    fn image_paste_is_current(&self, ticket: &ImagePasteTicket) -> Result<(), &'static str> {
        if self.document_instance(ticket.id) != Some(ticket.instance) {
            return Err("文書が閉じられたか入れ替わりました");
        }
        if self.sessions.active_id() != ticket.id {
            return Err("別のタブに切り替わりました");
        }
        if !self.body_editable() || self.external_comparison_blocks_body() {
            return Err("本文を編集できない状態になりました");
        }
        if !matches!(self.input_target(), InputTarget::Body)
            || self.content_search_input_is_focused()
            || self.content_search_results_focused()
            || self.document_find_input_is_focused()
            || self.ai_selection_blocks_editor_keys()
        {
            return Err("別の入力欄へ移りました");
        }
        let session = self.sessions.active();
        if session.editor().ime().is_some() {
            return Err("文字の変換中です");
        }
        if session.revision() != ticket.revision {
            return Err("本文が変更されました");
        }
        if session.editor().selection() != ticket.selection {
            return Err("選択範囲が変わりました");
        }
        if self.document_base_directory(ticket.id).as_deref() != Some(ticket.base.as_path())
            || self.work_folder_drafts.get(&ticket.id).map(|draft| draft.draft_id) != ticket.draft
        {
            return Err("保存先が変わりました");
        }
        Ok(())
    }

    /// Removes a file this job created and never published.
    fn discard_unpublished_image(&self, path: PathBuf, cx: &mut Context<Self>) {
        cx.background_executor()
            .spawn(async move {
                let _ = std::fs::remove_file(path);
            })
            .detach();
    }

    fn finish_image_paste(
        &mut self,
        ticket: ImagePasteTicket,
        shape: InsertionShape,
        outcome: Result<PastedImage, ImagePasteError>,
        cx: &mut Context<Self>,
    ) {
        let newest = self.image_paste.latest_job == ticket.job;
        let pasted = match outcome {
            Ok(pasted) => pasted,
            Err(error) => {
                // Nothing was inserted and the selection is untouched; the
                // text on the clipboard is deliberately not used instead.
                if newest {
                    self.status = Some(format!("画像を貼り付けられませんでした: {error}"));
                }
                cx.notify();
                return;
            }
        };
        if let Err(reason) = self.image_paste_is_current(&ticket) {
            self.discard_unpublished_image(pasted.saved.path, cx);
            if newest {
                self.status = Some(format!("画像の貼り付けを中止しました（{reason}）"));
            }
            cx.notify();
            return;
        }
        let replacement = shape.replacement(&clip::image_reference(&pasted.saved.file_name));
        let range = ticket.selection.range();
        let caret = SourceOffset(range.start.0 + replacement.len());
        // No composition is running (checked above), so the commit inside
        // `replace_range_recorded` cannot finalise or cancel an IME session.
        match self
            .editor_mut()
            .replace_range_recorded(range, &replacement, Selection::caret(caret))
        {
            Ok(_) => {
                self.status = Some(format!(
                    "画像を貼り付けました（{}×{}）",
                    pasted.width, pasted.height
                ));
                self.after_input(cx);
            }
            Err(error) => {
                self.discard_unpublished_image(pasted.saved.path, cx);
                self.report_error("image paste", error);
            }
        }
        cx.notify();
    }

    /// Called when a save of `id` landed: an image paste that waited for the
    /// first save of an untitled document continues, if nothing changed.
    pub(super) fn resume_image_paste_after_save(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(pending) = self.image_paste.pending_save.take_if(|pending| pending.id == id)
        else {
            return;
        };
        let unchanged = self.document_instance(id) == Some(pending.instance)
            && self.sessions.active_id() == id
            && self.sessions.active().revision() == pending.revision
            && self.editor().selection() == pending.selection;
        if unchanged {
            self.paste_clipboard_image(pending.raster, cx);
        } else {
            self.status = Some("保存後に文書が変わったため、画像の貼り付けを中止しました".to_owned());
            cx.notify();
        }
    }

    /// Drops a waiting image paste of `id`: the save it waited for did not
    /// happen, so nothing was created and the document is unchanged.
    pub(super) fn forget_pending_image_paste(&mut self, id: SessionId) {
        let _ = self
            .image_paste
            .pending_save
            .take_if(|pending| pending.id == id);
    }

    pub(super) fn forget_all_pending_image_pastes(&mut self) {
        self.image_paste.pending_save = None;
    }

    /// Save As to `path`. When the document refers to Hane-generated images and
    /// the target directory differs, the images are copied next to the new file
    /// first, and the document is written only after the copy succeeded.
    pub(super) fn save_as_to(
        &mut self,
        id: SessionId,
        path: PathBuf,
        close_after_save: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((ticket, names)) = self.plan_generated_image_copy(id, &path, close_after_save)
        else {
            self.write_save_as(id, path, close_after_save, cx);
            return;
        };
        let from = ticket.from.clone();
        let to = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.status = Some("画像をコピーしています…".to_owned());
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    clip::copy_generated_images(&from, &to, &names, &mut clip::generated_file_name)
                })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_save_as_copy(ticket, result, cx));
        })
        .detach();
        cx.notify();
    }

    fn plan_generated_image_copy(
        &self,
        id: SessionId,
        path: &Path,
        close_after_save: bool,
    ) -> Option<(SaveAsCopyTicket, Vec<String>)> {
        let session = self.sessions.get(id)?;
        let instance = self.document_instance(id)?;
        let from = self.document_base_directory(id)?;
        if path.parent()? == from {
            return None;
        }
        let text = session.editor().document().full_text();
        let names: Vec<String> = clip::generated_references(&text)
            .into_iter()
            .map(|reference| reference.file_name)
            .collect();
        if names.is_empty() {
            return None;
        }
        Some((
            SaveAsCopyTicket {
                id,
                instance,
                revision: session.revision(),
                path: path.to_path_buf(),
                close_after_save,
                from,
            },
            names,
        ))
    }

    fn write_save_as(
        &mut self,
        id: SessionId,
        path: PathBuf,
        close_after_save: bool,
        cx: &mut Context<Self>,
    ) {
        if close_after_save && let Some(instance) = self.document_instance(id) {
            // Armed against the instance as of this specific write, so only
            // this user-chosen write's own landing can close the tab.
            self.tab_close_after_save.insert(id, instance);
        }
        self.save_session(id, SaveIntent::To(path), cx);
    }

    fn finish_save_as_copy(
        &mut self,
        ticket: SaveAsCopyTicket,
        result: Result<clip::CopiedImages, ImagePasteError>,
        cx: &mut Context<Self>,
    ) {
        let same_instance = self.document_instance(ticket.id) == Some(ticket.instance);
        let current = same_instance
            && self
                .sessions
                .get(ticket.id)
                .is_some_and(|session| session.revision() == ticket.revision)
            && self.document_base_directory(ticket.id).as_deref() == Some(ticket.from.as_path());
        let copied = match result {
            Ok(copied) => copied,
            Err(error) => {
                // Nothing was saved and nothing the copy created is left; the
                // text, the images and the unsaved state are as they were.
                if same_instance {
                    self.status = Some(format!(
                        "画像をコピーできなかったため、保存しませんでした: {error}"
                    ));
                }
                cx.notify();
                return;
            }
        };
        let abandon = |view: &mut Self, message: &str, cx: &mut Context<Self>| {
            for path in &copied.created {
                view.discard_unpublished_image(path.clone(), cx);
            }
            if same_instance {
                view.status = Some(message.to_owned());
            }
            cx.notify();
        };
        if !current {
            abandon(
                self,
                "保存前に文書が変更されたため、保存を中止しました。もう一度保存してください",
                cx,
            );
            return;
        }
        if !copied.renamed.is_empty() && !self.rewrite_copied_references(&ticket, &copied.renamed, cx)
        {
            abandon(
                self,
                "画像の参照を更新できなかったため、保存しませんでした",
                cx,
            );
            return;
        }
        self.write_save_as(ticket.id, ticket.path, ticket.close_after_save, cx);
    }

    /// Points the references whose image had to be renamed in the new
    /// directory at the new names, as recorded replacements. Only the active
    /// document can be edited here; `false` means nothing may be saved.
    fn rewrite_copied_references(
        &mut self,
        ticket: &SaveAsCopyTicket,
        renamed: &[(String, String)],
        cx: &mut Context<Self>,
    ) -> bool {
        if self.sessions.active_id() != ticket.id || !self.body_editable() {
            return false;
        }
        let text = self.editor().document().full_text();
        let edits: Vec<(Range<usize>, String)> = clip::generated_references(&text)
            .into_iter()
            .filter_map(|reference| {
                renamed
                    .iter()
                    .find(|(old, _)| *old == reference.file_name)
                    .map(|(_, new)| (reference.name_range, new.clone()))
            })
            .collect();
        // From the end of the document so earlier ranges stay valid.
        for (range, name) in edits.into_iter().rev() {
            let caret = Selection::caret(SourceOffset(range.start + name.len()));
            let replaced = self.editor_mut().replace_range_recorded(
                SourceRange::new(range.start, range.end),
                &name,
                caret,
            );
            if replaced.is_err() {
                return false;
            }
        }
        self.after_input(cx);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type TestView = gpui::Entity<EditorView>;
    type TestCx = gpui::VisualTestContext;

    fn temp_root(label: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-image-paste-ui-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn png(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(pixel));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn image_item(text: Option<&str>, bytes: Vec<u8>) -> ClipboardItem {
        let mut item = match text {
            Some(text) => ClipboardItem::new_string(text.to_owned()),
            None => ClipboardItem {
                entries: Vec::new(),
            },
        };
        let id = gpui::hash(&bytes);
        item.entries
            .push(gpui::ClipboardEntry::Image(gpui::Image {
                format: gpui::ImageFormat::Png,
                bytes,
                id,
            }));
        item
    }

    fn open_file_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        directory: &Path,
        body: &str,
    ) -> (TestView, &'a mut TestCx) {
        cx.update(crate::actions::register_key_bindings);
        std::fs::create_dir_all(directory).unwrap();
        let path = directory.join("note.md");
        std::fs::write(&path, body).unwrap();
        let loaded = OsFileService.load(&path).unwrap();
        let (view, cx) = cx.add_window_view(move |_, cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(loaded),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();
        // The formal block index is published after a short debounce; the
        // paragraph check needs it.
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| window.focus(&view.focus_handle, cx));
        });
        (view, cx)
    }

    fn select(view: &TestView, cx: &mut TestCx, anchor: usize, active: usize) {
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(anchor),
                    active: SourceOffset(active),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();
    }

    fn text_of(view: &TestView, cx: &TestCx) -> String {
        view.read_with(cx, |view, _| view.editor().document().full_text())
    }

    fn status_of(view: &TestView, cx: &TestCx) -> Option<String> {
        view.read_with(cx, |view, _| view.status.clone())
    }

    fn pasted_files(directory: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(directory.join("images")) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries.map(|entry| entry.unwrap().path()).collect();
        files.sort();
        files
    }

    fn reference_in(text: &str) -> String {
        let found = clip::generated_references(text);
        assert_eq!(found.len(), 1, "{text:?}");
        found[0].file_name.clone()
    }

    #[gpui::test]
    fn an_image_beats_its_text_and_is_one_undoable_replacement(cx: &mut gpui::TestAppContext) {
        let root = temp_root("paste");
        let directory = root.join("ノート 1 (a)").join("sub");
        let (view, cx) = open_file_view(cx, &directory, "hello\n");
        select(&view, cx, 6, 6);
        cx.write_to_clipboard(image_item(Some("cell text"), png(3, 2, [9, 8, 7, 0])));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();

        let text = text_of(&view, cx);
        let name = reference_in(&text);
        assert_eq!(text, format!("hello\n\n![画像](images/{name})\n"));
        assert!(!text.contains("cell text"));
        let files = pasted_files(&directory);
        assert_eq!(files.len(), 1);
        let decoded = image::open(&files[0]).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (3, 2));
        assert_eq!(decoded.get_pixel(0, 0).0[3], 0, "alpha is kept");

        cx.simulate_keystrokes("secondary-z");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "hello\n");
        assert!(files[0].is_file(), "undo never deletes the image");
        cx.simulate_keystrokes("secondary-shift-z");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), text);
        assert_eq!(pasted_files(&directory), files);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_selection_is_replaced_and_one_undo_restores_it(cx: &mut gpui::TestAppContext) {
        let root = temp_root("selection");
        let (view, cx) = open_file_view(cx, &root, "before TARGET after\n");
        select(&view, cx, 7, 13);
        cx.write_to_clipboard(image_item(None, png(1, 1, [1, 2, 3, 255])));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        let text = text_of(&view, cx);
        let name = reference_in(&text);
        assert_eq!(text, format!("before \n\n![画像](images/{name})\n\n after\n"));
        cx.simulate_keystrokes("secondary-z");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "before TARGET after\n");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_clipboard_without_an_image_pastes_text_as_before(cx: &mut gpui::TestAppContext) {
        let root = temp_root("text");
        let (view, cx) = open_file_view(cx, &root, "ab\n");
        select(&view, cx, 1, 1);
        cx.write_to_clipboard(ClipboardItem::new_string("XY".to_owned()));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "aXYb\n");
        assert!(!root.join("images").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn an_svg_is_not_a_raster_image_and_the_text_is_pasted(cx: &mut gpui::TestAppContext) {
        let root = temp_root("svg");
        let (view, cx) = open_file_view(cx, &root, "ab\n");
        select(&view, cx, 1, 1);
        let mut item = ClipboardItem::new_string("XY".to_owned());
        item.entries.push(gpui::ClipboardEntry::Image(gpui::Image {
            format: gpui::ImageFormat::Svg,
            bytes: b"<svg/>".to_vec(),
            id: 1,
        }));
        cx.write_to_clipboard(item);
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "aXYb\n");
        assert!(!root.join("images").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn an_undecodable_image_keeps_the_selection_and_never_falls_back_to_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("corrupt");
        let (view, cx) = open_file_view(cx, &root, "keep this\n");
        select(&view, cx, 0, 4);
        cx.write_to_clipboard(image_item(Some("fallback text"), b"not a png".to_vec()));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "keep this\n");
        assert!(status_of(&view, cx).unwrap().contains("貼り付けられませんでした"));
        assert!(pasted_files(&root).is_empty());
        let selection = view.read_with(cx, |view, _| view.editor().selection());
        assert_eq!(selection.range(), SourceRange::new(0, 4));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn images_that_is_a_file_fails_without_touching_the_document(cx: &mut gpui::TestAppContext) {
        let root = temp_root("images-file");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        std::fs::write(root.join("images"), b"i am a file").unwrap();
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "body\n");
        assert_eq!(std::fs::read(root.join("images")).unwrap(), b"i am a file");
        assert!(status_of(&view, cx).unwrap().contains("images"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_position_that_is_not_a_paragraph_is_refused_before_any_file_is_made(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("heading");
        let (view, cx) = open_file_view(cx, &root, "# Title\n\n- item\n\n```\ncode\n```\n");
        for offset in [3, 11, 22] {
            select(&view, cx, offset, offset);
            cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
            cx.simulate_keystrokes("secondary-v");
            cx.run_until_parked();
            assert_eq!(
                text_of(&view, cx),
                "# Title\n\n- item\n\n```\ncode\n```\n",
                "offset {offset}"
            );
            assert!(status_of(&view, cx).unwrap().contains("段落"), "offset {offset}");
        }
        assert!(!root.join("images").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_read_only_body_does_not_start_a_job(cx: &mut gpui::TestAppContext) {
        let root = temp_root("reading");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.set_editor_mode(EditorMode::Reading, window, cx));
            });
        });
        cx.write_to_clipboard(image_item(Some("t"), png(1, 1, [0, 0, 0, 255])));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "body\n");
        assert!(!root.join("images").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn paste_without_waiting(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |view, cx| {
            let snapshot = ClipboardSnapshot::read(cx);
            view.paste_into_body(snapshot, cx);
        });
    }

    #[gpui::test]
    fn a_body_edit_while_the_image_is_saved_aborts_and_removes_the_new_file(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("stale-edit");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        paste_without_waiting(&view, cx);
        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("!").unwrap();
            view.after_input(cx);
        });
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "body!\n");
        assert!(pasted_files(&root).is_empty(), "the unpublished file is removed");
        assert!(status_of(&view, cx).unwrap().contains("中止"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_selection_change_while_the_image_is_saved_aborts(cx: &mut gpui::TestAppContext) {
        let root = temp_root("stale-selection");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        paste_without_waiting(&view, cx);
        select(&view, cx, 0, 0);
        assert_eq!(text_of(&view, cx), "body\n");
        assert!(pasted_files(&root).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_tab_switch_while_the_image_is_saved_never_edits_another_document(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("stale-tab");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        paste_without_waiting(&view, cx);
        view.update(cx, |view, cx| {
            view.sessions.open_untitled("other\n", "Other");
            view.on_document_replaced();
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "other\n");
        assert!(pasted_files(&root).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn an_in_place_reload_while_the_image_is_saved_aborts(cx: &mut gpui::TestAppContext) {
        let root = temp_root("stale-adopt");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        paste_without_waiting(&view, cx);
        let loaded = OsFileService.load(&root.join("note.md")).unwrap();
        view.update(cx, |view, cx| {
            view.sessions.active_mut().adopt(loaded);
            view.on_document_replaced();
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "body\n");
        assert!(pasted_files(&root).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_workspace_change_while_the_image_is_saved_aborts(cx: &mut gpui::TestAppContext) {
        let root = temp_root("stale-workspace");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        paste_without_waiting(&view, cx);
        view.update(cx, |view, cx| {
            view.work_folder_generation += 1;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "body\n");
        assert!(pasted_files(&root).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn two_quick_pastes_apply_the_first_and_safely_abort_the_second(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("two-pastes");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        paste_without_waiting(&view, cx);
        paste_without_waiting(&view, cx);
        cx.run_until_parked();
        let text = text_of(&view, cx);
        reference_in(&text);
        assert_eq!(pasted_files(&root).len(), 1, "the aborted job's file is removed");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_workspace_draft_uses_its_target_directory_as_the_base(cx: &mut gpui::TestAppContext) {
        let root = temp_root("draft");
        let target = root.join("folder");
        std::fs::create_dir_all(&target).unwrap();
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.work_folder_drafts.insert(
                id,
                WorkFolderDraft {
                    draft_id: DraftId::generate(),
                    target_directory: target.clone(),
                },
            );
            assert_eq!(view.document_base_directory(id), Some(target.clone()));
            assert_eq!(
                view.document_resource_resolver().base(),
                Some(target.as_path())
            );
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| window.focus(&view.focus_handle, cx));
        });
        cx.write_to_clipboard(image_item(None, png(2, 2, [5, 5, 5, 255])));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        let text = text_of(&view, cx);
        let name = reference_in(&text);
        assert_eq!(text, format!("![画像](images/{name})\n"));
        assert_eq!(pasted_files(&target).len(), 1);
        assert!(!root.join("images").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_standalone_untitled_document_has_no_base_and_keeps_its_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("keep\n", "Untitled", cx));
        view.update(cx, |view, _| {
            let id = view.sessions.active_id();
            assert_eq!(view.document_base_directory(id), None);
            assert_eq!(view.document_resource_resolver().base(), None);
        });
    }

    fn save_as(view: &TestView, cx: &mut TestCx, path: PathBuf) {
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.save_as_to(id, path, false, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn save_as_to_another_directory_copies_the_generated_images_first(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let name = "hane-paste-1-0123abcd.png";
        std::fs::write(from.join("images").join(name), b"png-bytes").unwrap();
        let body = format!("![画像](images/{name})\n![手書き](images/photo.png)\n");
        let (view, cx) = open_file_view(cx, &from, &body);
        save_as(&view, cx, to.join("note.md"));
        assert_eq!(std::fs::read(to.join("images").join(name)).unwrap(), b"png-bytes");
        assert!(from.join("images").join(name).is_file(), "the source is kept");
        assert!(!to.join("images").join("photo.png").exists());
        assert_eq!(std::fs::read_to_string(to.join("note.md")).unwrap(), body);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn save_as_renames_a_conflicting_image_and_updates_the_reference(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-conflict");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(to.join("images")).unwrap();
        let name = "hane-paste-1-0123abcd.png";
        std::fs::write(from.join("images").join(name), b"mine").unwrap();
        std::fs::write(to.join("images").join(name), b"someone else's").unwrap();
        let (view, cx) = open_file_view(cx, &from, &format!("![画像](images/{name})\n"));
        save_as(&view, cx, to.join("note.md"));
        let saved = std::fs::read_to_string(to.join("note.md")).unwrap();
        let new_name = reference_in(&saved);
        assert_ne!(new_name, name);
        assert_eq!(saved, format!("![画像](images/{new_name})\n"));
        assert_eq!(std::fs::read(to.join("images").join(name)).unwrap(), b"someone else's");
        assert_eq!(std::fs::read(to.join("images").join(&new_name)).unwrap(), b"mine");
        assert_eq!(text_of(&view, cx), saved);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_failed_image_copy_saves_nothing_and_keeps_the_unsaved_document(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-fail");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let name = "hane-paste-1-0123abcd.png";
        // A directory in place of the image cannot be read as a file.
        std::fs::create_dir(from.join("images").join(name)).unwrap();
        let body = format!("![画像](images/{name})\n");
        let (view, cx) = open_file_view(cx, &from, &body);
        select(&view, cx, 0, 0);
        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("x").unwrap();
            view.after_input(cx);
        });
        save_as(&view, cx, to.join("note.md"));
        assert!(!to.join("note.md").exists(), "nothing is saved");
        assert!(status_of(&view, cx).unwrap().contains("コピーできなかった"));
        assert!(view.read_with(cx, |view, _| view.sessions.active().is_dirty()));
        assert_eq!(text_of(&view, cx), format!("x{body}"));
        assert!(from.join("images").join(name).is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_document_edit_during_the_copy_abandons_the_save_and_the_new_copies(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-stale");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let name = "hane-paste-1-0123abcd.png";
        std::fs::write(from.join("images").join(name), b"png").unwrap();
        let (view, cx) = open_file_view(cx, &from, &format!("![画像](images/{name})\n"));
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.save_as_to(id, to.join("note.md"), false, cx);
            view.editor_mut().insert_text("x").unwrap();
            view.after_input(cx);
        });
        cx.run_until_parked();
        assert!(!to.join("note.md").exists());
        assert!(!to.join("images").join(name).exists(), "the new copy is removed");
        assert!(from.join("images").join(name).is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_snapshot_prefers_a_raster_and_keeps_the_text() {
        let snapshot = ClipboardSnapshot::from_item(&image_item(Some("t"), png(1, 1, [0; 4])));
        assert_eq!(snapshot.text(), Some("t"));
        assert!(snapshot.raster.is_some());
        assert!(snapshot.has_paste_content());
        let empty = ClipboardSnapshot::from_item(&ClipboardItem::new_string(String::new()));
        assert!(!empty.has_paste_content());
    }
}
