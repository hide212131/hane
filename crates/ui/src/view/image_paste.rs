//! Clipboard image paste (Issue #591), the GPUI side.
//!
//! The body Paste path reads one [`ClipboardSnapshot`] containing plain text,
//! HTML and images from the same clipboard generation. A real raster image wins
//! for rich Paste; text and HTML are handled by the shared paste dispatcher.
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

/// Why the clipboard could not be read into something to paste. Mirrors the
/// error of the platform crates' rich clipboard read, so a busy, changing or
/// invalid clipboard is never mistaken for an empty one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(test, target_os = "macos", windows)), allow(dead_code))]
pub(crate) enum ClipboardReadError {
    Busy,
    ChangedDuringRead,
    InvalidFormat,
    TooLarge,
}

impl ClipboardReadError {
    pub(super) fn reason(self) -> &'static str {
        match self {
            Self::Busy => {
                "クリップボードを他のアプリが使用中で読み取れません。少し待ってからもう一度貼り付けてください"
            }
            Self::ChangedDuringRead => {
                "読み取り中にクリップボードが変更されました。もう一度貼り付けてください"
            }
            Self::InvalidFormat => "クリップボードの画像データが正しくありません",
            Self::TooLarge => "クリップボードの画像が大きすぎます",
        }
    }
}

/// An image entry that could not be read, with the format it declared when the
/// reader knows it. An unsupported (SVG) entry is not a raster image, so its
/// failure is not an image failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(test, target_os = "macos", windows)), allow(dead_code))]
pub(crate) struct ImageReadFailure {
    error: ClipboardReadError,
    format: Option<gpui::ImageFormat>,
}

impl From<ClipboardReadError> for ImageReadFailure {
    fn from(error: ClipboardReadError) -> Self {
        Self {
            error,
            format: None,
        }
    }
}

/// What the clipboard offers as an image. One OS item is one image however
/// many formats it carries; independent OS items are never merged or cut down
/// to the first one.
#[derive(Debug, Default)]
enum ClipboardImages {
    /// No raster image (an SVG alone is not one).
    #[default]
    Empty,
    One(Arc<ClipboardRaster>),
    /// This many independent images; none can be chosen for the user.
    Several(usize),
    /// The one image could not be read.
    Unreadable(ClipboardReadError),
}

/// The platform-neutral form of one native clipboard read: the text and file
/// paths, and one entry per independent image item.
#[cfg_attr(not(any(test, target_os = "macos", windows)), allow(dead_code))]
pub(crate) struct NativeRead {
    item: ClipboardItem,
    html: Option<String>,
    html_problem: bool,
    images: Vec<Result<gpui::Image, ImageReadFailure>>,
}

/// Everything one Paste needs from the OS clipboard, read in a single call so
/// text, HTML and images always come from the same clipboard generation.
#[derive(Debug, Default)]
pub(crate) struct ClipboardSnapshot {
    pub(super) text: Option<String>,
    pub(super) html: Option<String>,
    pub(super) html_problem: bool,
    images: ClipboardImages,
    /// The whole read failed; there is nothing to paste and nothing to fall
    /// back to.
    pub(super) read_error: Option<ClipboardReadError>,
}

impl ClipboardSnapshot {
    fn from_images(
        text: Option<String>,
        images: Vec<Result<gpui::Image, ImageReadFailure>>,
    ) -> Self {
        let mut candidates: Vec<Result<Arc<ClipboardRaster>, ClipboardReadError>> = images
            .into_iter()
            .filter_map(|image| match image {
                Ok(source) => clip::source_format(source.format).map(|format| {
                    Ok(Arc::new(ClipboardRaster {
                        format,
                        bytes: source.bytes,
                    }))
                }),
                // An unreadable SVG is not an image failure; the text pastes.
                Err(ImageReadFailure {
                    format: Some(gpui::ImageFormat::Svg),
                    ..
                }) => None,
                Err(failure) => Some(Err(failure.error)),
            })
            .collect();
        let images = match candidates.len() {
            0 => ClipboardImages::Empty,
            1 => match candidates.remove(0) {
                Ok(raster) => ClipboardImages::One(raster),
                Err(error) => ClipboardImages::Unreadable(error),
            },
            count => ClipboardImages::Several(count),
        };
        Self {
            text,
            html: None,
            html_problem: false,
            images,
            read_error: None,
        }
    }

    /// A GPUI `ClipboardItem` cannot say which images are alternative formats
    /// of one OS item, so each of its image entries counts as its own item.
    /// The native reads below collapse the formats of one item beforehand.
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    pub(crate) fn from_item(item: &ClipboardItem) -> Self {
        let images = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                gpui::ClipboardEntry::Image(source) => Some(Ok(gpui::Image {
                    format: source.format,
                    bytes: source.bytes.clone(),
                    id: source.id,
                })),
                _ => None,
            })
            .collect();
        Self::from_images(item.text(), images)
    }

    /// The outcome of a native rich read: `Ok(None)` is a really empty
    /// clipboard, every failure keeps its identity.
    #[cfg_attr(not(any(test, target_os = "macos", windows)), allow(dead_code))]
    pub(crate) fn from_native(read: Result<Option<NativeRead>, ClipboardReadError>) -> Self {
        match read {
            Ok(Some(read)) => {
                let mut snapshot = Self::from_images(read.item.text(), read.images);
                snapshot.html = read.html;
                snapshot.html_problem = read.html_problem;
                snapshot
            }
            Ok(None) => Self::default(),
            Err(error) => Self {
                read_error: Some(error),
                ..Self::default()
            },
        }
    }

    /// macOS and Windows: the patched platform crate's single snapshot.
    #[cfg(all(not(test), any(target_os = "macos", windows)))]
    pub(crate) fn read(_cx: &gpui::App) -> Self {
        Self::from_native(native::read())
    }

    /// Other targets, and the GPUI test clipboard, which only has the
    /// `Option` of GPUI's own read (an empty and a failed read look alike).
    #[cfg(not(all(not(test), any(target_os = "macos", windows))))]
    pub(crate) fn read(cx: &gpui::App) -> Self {
        let item = cx.read_from_clipboard();
        #[cfg(test)]
        let html = crate::clipboard::test_html_snapshot();
        #[cfg(not(test))]
        let html = Ok((None, false));
        match html {
            Ok((html, html_problem)) => {
                let mut snapshot = item.as_ref().map_or_else(Self::default, Self::from_item);
                snapshot.html = html;
                snapshot.html_problem = html_problem;
                snapshot
            }
            Err(error) => Self {
                read_error: Some(match error {
                    crate::clipboard::SnapshotError::Busy => ClipboardReadError::Busy,
                    crate::clipboard::SnapshotError::ChangedDuringRead => {
                        ClipboardReadError::ChangedDuringRead
                    }
                }),
                ..Self::default()
            },
        }
    }

    pub(crate) fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Whether a body Paste would do something with this snapshot.
    pub(crate) fn has_paste_content(&self) -> bool {
        !matches!(self.images, ClipboardImages::Empty)
            || self.text.as_deref().is_some_and(|text| !text.is_empty())
            || self.html.is_some()
    }
}

#[cfg(all(not(test), any(target_os = "macos", windows)))]
mod native {
    use super::{ClipboardReadError, NativeRead};
    #[cfg(target_os = "macos")]
    use gpui_macos as platform;
    #[cfg(windows)]
    use gpui_windows as platform;

    use platform::RichClipboardError as Native;

    fn error(error: Native) -> ClipboardReadError {
        match error {
            Native::Busy => ClipboardReadError::Busy,
            Native::ChangedDuringRead => ClipboardReadError::ChangedDuringRead,
            Native::InvalidFormat => ClipboardReadError::InvalidFormat,
            Native::TooLarge => ClipboardReadError::TooLarge,
        }
    }

    pub(super) fn read() -> Result<Option<NativeRead>, ClipboardReadError> {
        let snapshot = platform::read_rich_clipboard().map_err(error)?;
        Ok(snapshot.map(|snapshot| NativeRead {
            item: snapshot.item,
            html: snapshot.html,
            html_problem: snapshot.html_problem.is_some(),
            images: snapshot
                .images
                .into_iter()
                .map(|image| image.map_err(|failure| error(failure).into()))
                .collect(),
        }))
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
    clipboard_generation: u64,
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
    /// The destination the user chose in this paste's own Save As prompt. Only
    /// the write to that destination may resume (or drop) the paste; it stays
    /// `None` while the prompt is unanswered.
    save_path: Option<PathBuf>,
    /// The write this paste's own Save As request started. Set when that
    /// request is accepted as a write (immediately, or once an earlier running
    /// write finished), so a completion of any earlier write to the same
    /// destination cannot be mistaken for it.
    save_ticket: Option<SaveTicket>,
}

#[derive(Default)]
pub(super) struct ImagePasteState {
    /// Job number of the newest paste request. A result (and the status text
    /// it may clear) only counts for the job that is still the newest, so an
    /// older job can neither overwrite nor clear a newer job's message.
    latest_job: u64,
    pending_save: Option<PendingImagePaste>,
    /// Copies a running Save As created, by the document that asked for it.
    /// Dropped (and the copies removed unless something refers to them) when
    /// that exact write does not land.
    save_as_copies: std::collections::HashMap<SessionId, SaveAsCopies>,
}

/// The images a running Save As created in the target directory.
struct SaveAsCopies {
    instance: DocumentInstance,
    path: PathBuf,
    created: Vec<PathBuf>,
}

/// The rewrite that points the references of renamed images at their new
/// names. It reads the text alone, so the same plan serves the saved text,
/// the live document and every state of its edit history.
fn rename_plan(renamed: Vec<(String, String)>) -> Arc<hane_editor::RewritePlan> {
    Arc::new(move |text: &str| {
        clip::generated_references(text)
            .into_iter()
            .filter_map(|reference| {
                renamed
                    .iter()
                    .find(|(old, _)| *old == reference.file_name)
                    .map(|(_, new)| (reference.name_range, new.clone()))
            })
            .collect()
    })
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

    /// Handles the image part of a shared clipboard snapshot for rich Paste.
    /// Returns true when the snapshot has an image or a read failure, so text
    /// handling cannot silently replace the selection after an image failure.
    pub(super) fn paste_image_from_snapshot(
        &mut self,
        snapshot: &ClipboardSnapshot,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(error) = snapshot.read_error {
            self.refuse_clipboard_paste("貼り付けられませんでした", error.reason(), cx);
            return true;
        }
        match &snapshot.images {
            ClipboardImages::One(raster) => self.paste_clipboard_image(Arc::clone(raster), cx),
            ClipboardImages::Several(count) => {
                let reason = format!(
                    "クリップボードに独立した画像が{count}件あります。画像は1枚ずつコピーして貼り付けてください"
                );
                self.refuse_clipboard_paste("画像を貼り付けられませんでした", &reason, cx);
            }
            ClipboardImages::Unreadable(error) => {
                self.refuse_clipboard_paste("画像を貼り付けられませんでした", error.reason(), cx);
            }
            ClipboardImages::Empty => return false,
        }
        true
    }

    fn refuse_clipboard_paste(&mut self, what: &str, reason: &str, cx: &mut Context<Self>) {
        // A read-only body ignores a Paste without a word, as it always did.
        if !self.body_editable() || self.external_comparison_blocks_body() {
            return;
        }
        self.status = Some(format!("{what}: {reason}"));
        cx.notify();
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
                save_path: None,
                save_ticket: None,
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
            clipboard_generation: self.clipboard_generation,
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
            // The outcome stays reachable from this task until the view has
            // taken it, so a view released before delivery (the update closure
            // is then dropped unrun) still lets this job remove its own file.
            let slot = Arc::new(std::sync::Mutex::new(Some(outcome)));
            let delivered = view.update(cx, {
                let slot = Arc::clone(&slot);
                move |view, cx| {
                    let outcome = slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if let Some(outcome) = outcome {
                        view.finish_image_paste(ticket, shape, outcome, cx);
                    }
                }
            });
            if delivered.is_err() {
                let undelivered = slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                if let Some(Ok(pasted)) = undelivered {
                    let path = pasted.saved.path;
                    cx.background_executor()
                        .spawn(async move {
                            let _ = std::fs::remove_file(path);
                        })
                        .await;
                }
            }
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
            // 選択は終端を含まないため、非空選択では最後に選択されたbyteの属するブロックまで検査する。
            let last_offset = if range.end.0 > range.start.0 {
                SourceOffset(range.end.0 - 1)
            } else {
                range.end
            };
            let (Some(first), Some(last)) = (ordinal_for(range.start), ordinal_for(last_offset))
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
        if self.clipboard_generation != ticket.clipboard_generation {
            return Err("別のクリップボード操作が始まりました");
        }
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
        if self.document_base_directory(ticket.id).as_deref() != Some(ticket.base.as_path()) {
            return Err("保存先が変わりました");
        }
        // A Save As to another directory that is still writing has already
        // planned which images to copy; an image added now would be missing
        // from the destination once that write lands.
        if self
            .sessions
            .get(ticket.id)
            .is_some_and(|session| session.writes_outside_directory(&ticket.base))
        {
            return Err("別の場所への保存が進行中です");
        }
        let current_draft = self.work_folder_drafts.get(&ticket.id).map(|draft| draft.draft_id);
        if current_draft != ticket.draft {
            // The one accepted change is a draft's first save into the very
            // directory it was going to be created in: the draft entry is
            // retired and the document now has its own file there. The base
            // check above already pins that directory; the file itself must
            // be what provides it, so a vanished draft never passes.
            let first_saved_in_place = ticket.draft.is_some()
                && current_draft.is_none()
                && self
                    .sessions
                    .get(ticket.id)
                    .and_then(|session| session.file().directory())
                    == Some(ticket.base.as_path());
            if !first_saved_in_place {
                return Err("保存先が変わりました");
            }
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
    /// Only the write the paste's own prompt started counts.
    pub(super) fn resume_image_paste_after_save(
        &mut self,
        id: SessionId,
        ticket: SaveTicket,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self
            .image_paste
            .pending_save
            .take_if(|pending| pending.id == id && pending.save_ticket == Some(ticket))
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

    /// A write did not land: drops the waiting paste only when that write is
    /// the one its own prompt started.
    pub(super) fn forget_pending_image_paste_for_save(
        &mut self,
        id: SessionId,
        ticket: SaveTicket,
    ) {
        let _ = self
            .image_paste
            .pending_save
            .take_if(|pending| pending.id == id && pending.save_ticket == Some(ticket));
    }

    /// A write to `path` was just accepted for `id`: when the waiting paste's
    /// own prompt chose `path` and no write was claimed yet, this is the
    /// paste's write. An earlier write started before the prompt was answered
    /// never reaches here after the binding, so it is never claimed.
    pub(super) fn claim_pending_image_paste_write(
        &mut self,
        id: SessionId,
        path: &Path,
        ticket: SaveTicket,
    ) {
        if let Some(pending) = self.image_paste.pending_save.as_mut()
            && pending.id == id
            && pending.save_ticket.is_none()
            && pending.save_path.as_deref() == Some(path)
        {
            pending.save_ticket = Some(ticket);
        }
    }

    /// The prompt opened for the waiting paste of `id` was answered with
    /// `path`; the write to it is the one that resumes the paste.
    pub(super) fn bind_pending_image_paste_save(&mut self, id: SessionId, path: &Path) {
        if let Some(pending) = self.image_paste.pending_save.as_mut()
            && pending.id == id
        {
            pending.save_path = Some(path.to_path_buf());
            pending.save_ticket = None;
        }
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
            self.write_save_as(id, path, close_after_save, None, cx);
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
        // References only Undo or Redo would bring back need their image in
        // the target directory just as much as the ones in the body now.
        let history = session.editor().history_texts().unwrap_or_default();
        let mut names: Vec<String> = Vec::new();
        for state in std::iter::once(&text).chain(history.iter()) {
            for reference in clip::generated_references(state) {
                if !names.contains(&reference.file_name) {
                    names.push(reference.file_name);
                }
            }
        }
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

    /// Starts the Save As write. With a `rewrite`, that one write saves the
    /// text as rewritten and the document follows only when it lands. `false`
    /// means no write was started and nothing was changed.
    fn write_save_as(
        &mut self,
        id: SessionId,
        path: PathBuf,
        close_after_save: bool,
        rewrite: Option<(Arc<hane_editor::RewritePlan>, Option<hane_editor::PreparedRewrite>)>,
        cx: &mut Context<Self>,
    ) -> bool {
        let carries_rewrite = rewrite.is_some();
        if let Some((plan, prepared)) = rewrite {
            let armed = self
                .sessions
                .get_mut(id)
                .is_some_and(|session| session.arm_prepared_save_rewrite(plan, prepared));
            if !armed {
                return false;
            }
        }
        if close_after_save && let Some(instance) = self.document_instance(id) {
            // Armed against the instance as of this specific write, so only
            // this user-chosen write's own landing can close the tab.
            self.tab_close_after_save.insert(id, instance);
        }
        let slot_was_busy = self
            .sessions
            .get(id)
            .is_some_and(DocumentSession::save_in_flight);
        self.save_session(id, SaveIntent::To(path), cx);
        if carries_rewrite
            && self
                .sessions
                .get_mut(id)
                .is_some_and(DocumentSession::disarm_save_rewrite)
        {
            // No write took the rewrite (a folder move blocked the save).
            self.tab_close_after_save.remove(&id);
            return false;
        }
        // Without a rewrite there is nothing to disarm, so a write that was
        // requested on a free save slot has started only when the slot is now
        // taken. A busy slot is not evidence about this request.
        if !slot_was_busy
            && !self
                .sessions
                .get(id)
                .is_some_and(DocumentSession::save_in_flight)
        {
            self.tab_close_after_save.remove(&id);
            return false;
        }
        true
    }

    /// Whether a document that is saved in, or being saved to, `directory`
    /// refers to the generated image `name`.
    fn generated_copy_in_use(&self, directory: &Path, name: &str) -> bool {
        let same_directory = |other: &Path| {
            other == directory
                || matches!(
                    (other.canonicalize(), directory.canonicalize()),
                    (Ok(a), Ok(b)) if a == b
                )
        };
        self.sessions.sessions().any(|session| {
            (session.file().directory().is_some_and(same_directory)
                || session.writes_under_folder(directory))
                && clip::generated_references(&session.editor().document().full_text())
                    .iter()
                    .any(|reference| reference.file_name == name)
        })
    }

    /// Removes copies a Save As created and nothing refers to. A copy another
    /// save has since published (reusing the identical file) stays.
    fn discard_unreferenced_copies(
        &self,
        directory: &Path,
        created: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        for path in created {
            let in_use = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| self.generated_copy_in_use(directory, name));
            if !in_use {
                self.discard_unpublished_image(path, cx);
            }
        }
    }

    /// Called with the result of every write of `id`. The copies a Save As
    /// created are kept only when that exact write (document instance and
    /// target) landed; otherwise they were never published.
    pub(super) fn settle_save_as_copies(
        &mut self,
        id: SessionId,
        instance: Option<DocumentInstance>,
        path: &Path,
        landed: bool,
        cx: &mut Context<Self>,
    ) {
        let owned = self
            .image_paste
            .save_as_copies
            .get(&id)
            .is_some_and(|copies| Some(copies.instance) == instance && copies.path == path);
        if !owned {
            return;
        }
        let Some(copies) = self.image_paste.save_as_copies.remove(&id) else {
            return;
        };
        if !landed && let Some(directory) = copies.path.parent() {
            self.discard_unreferenced_copies(directory, copies.created, cx);
        }
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
        if !current {
            self.abandon_save_as_copy(
                &ticket,
                &copied,
                same_instance,
                "保存前に文書が変更されたため、保存を中止しました。もう一度保存してください",
                cx,
            );
            return;
        }
        if copied.renamed.is_empty() {
            self.begin_save_as_write(ticket, copied, None, cx);
            return;
        }
        // Working out how every state of the edit history reads with the
        // references renamed costs document size times history depth, so it is
        // prepared in the background from a snapshot and only applied here,
        // while the document still is that snapshot. Anything edited in the
        // meantime is caught by the checks `begin_save_as_write` repeats.
        let plan = rename_plan(copied.renamed.clone());
        let snapshot = self
            .sessions
            .get(ticket.id)
            .and_then(|session| session.editor().rewrite_snapshot());
        let Some(snapshot) = snapshot else {
            // An open IME composition is not in a snapshot: the write
            // evaluates the plan itself.
            self.begin_save_as_write(ticket, copied, Some((plan, None)), cx);
            return;
        };
        self.status = Some("保存を準備しています…".to_owned());
        cx.spawn(async move |view, cx| {
            let background_plan = Arc::clone(&plan);
            let prepared = cx
                .background_executor()
                .spawn(async move { snapshot.prepare(&*background_plan) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.begin_save_as_write(ticket, copied, Some((plan, prepared)), cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Gives up a Save As whose copies were made: the copies nothing refers to
    /// are removed and the reason is shown for the document that asked.
    fn abandon_save_as_copy(
        &mut self,
        ticket: &SaveAsCopyTicket,
        copied: &clip::CopiedImages,
        same_instance: bool,
        message: &str,
        cx: &mut Context<Self>,
    ) {
        let destination = ticket.path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.discard_unreferenced_copies(&destination, copied.created.clone(), cx);
        if same_instance {
            self.status = Some(message.to_owned());
        }
        cx.notify();
    }

    /// Starts the write of a Save As whose images are copied. Everything that
    /// could have changed while the copy and the preparation ran in the
    /// background is checked again here, against the ticket.
    fn begin_save_as_write(
        &mut self,
        ticket: SaveAsCopyTicket,
        copied: clip::CopiedImages,
        rewrite: Option<(Arc<hane_editor::RewritePlan>, Option<hane_editor::PreparedRewrite>)>,
        cx: &mut Context<Self>,
    ) {
        let same_instance = self.document_instance(ticket.id) == Some(ticket.instance);
        let current = same_instance
            && self
                .sessions
                .get(ticket.id)
                .is_some_and(|session| session.revision() == ticket.revision)
            && self.document_base_directory(ticket.id).as_deref() == Some(ticket.from.as_path());
        if !current {
            self.abandon_save_as_copy(
                &ticket,
                &copied,
                same_instance,
                "保存前に文書が変更されたため、保存を中止しました。もう一度保存してください",
                cx,
            );
            return;
        }
        let copies_made = !copied.created.is_empty() || !copied.renamed.is_empty();
        // A write already running would queue this one behind it, and the
        // queued request can carry neither the rewrite nor the ownership of
        // the copies. Nothing is lost by saving again once it has landed.
        if copies_made
            && self
                .sessions
                .get(ticket.id)
                .is_some_and(DocumentSession::save_in_flight)
        {
            self.abandon_save_as_copy(
                &ticket,
                &copied,
                same_instance,
                "別の保存が実行中のため、保存を中止しました。完了後にもう一度保存してください",
                cx,
            );
            return;
        }
        // The text is not edited here: the write saves it with the references
        // pointed at the copies, and the document (history included) follows
        // when that write lands.
        let started = self.write_save_as(
            ticket.id,
            ticket.path.clone(),
            ticket.close_after_save,
            rewrite,
            cx,
        );
        if !started {
            self.abandon_save_as_copy(
                &ticket,
                &copied,
                same_instance,
                "保存を開始できなかったため、保存しませんでした",
                cx,
            );
            return;
        }
        if !copied.created.is_empty() {
            self.image_paste.save_as_copies.insert(
                ticket.id,
                SaveAsCopies {
                    instance: ticket.instance,
                    path: ticket.path,
                    created: copied.created.clone(),
                },
            );
        }
    }

    /// The document of `id` was rewritten by the Save As write that just
    /// landed; a view of it shows the new text.
    pub(super) fn refresh_after_save_rewrite(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if self.sessions.active_id() == id {
            self.after_input(cx);
        }
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
        crate::clipboard::set_test_snapshot(None, false, None);
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
        crate::clipboard::set_test_snapshot(Some("<b>cell html</b>"), false, None);
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();

        let text = text_of(&view, cx);
        let name = reference_in(&text);
        assert_eq!(text, format!("hello\n\n![画像](images/{name})\n"));
        assert!(!text.contains("cell text"));
        assert!(!text.contains("cell html"));
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
    fn a_selection_ending_at_the_next_heading_start_does_not_include_the_heading(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("exclusive-end");
        let original = "text\n\n# Heading\n";
        let (view, cx) = open_file_view(cx, &root, original);
        select(&view, cx, 0, 6);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        let text = text_of(&view, cx);
        let name = reference_in(&text);
        assert!(text.starts_with(&format!("![画像](images/{name})")), "{text:?}");
        assert!(text.ends_with("\n\n# Heading\n"), "{text:?}");
        assert!(!text.contains("text"), "{text:?}");
        cx.simulate_keystrokes("secondary-z");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), original);

        // 見出しの最初のbyteまで実際に選択している場合は引き続き拒否する。
        select(&view, cx, 0, 7);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        cx.simulate_keystrokes("secondary-v");
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), original);
        assert!(status_of(&view, cx).unwrap().contains("段落"));
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
            view.paste_image_from_snapshot(&snapshot, cx);
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
    fn a_view_released_before_delivery_still_removes_only_its_own_file(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("view-released");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        let other = root.join("images").join("unrelated.png");
        std::fs::create_dir_all(root.join("images")).unwrap();
        std::fs::write(&other, b"keep me").unwrap();
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        paste_without_waiting(&view, cx);
        // The background save has not run yet; release the view before its
        // result can be delivered.
        cx.update(|window, _| window.remove_window());
        drop(view);
        cx.run_until_parked();
        assert_eq!(pasted_files(&root), vec![other.clone()]);
        assert_eq!(std::fs::read(&other).unwrap(), b"keep me");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_saved_image_is_removed_when_the_view_is_released_before_its_delivery(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("saved-then-released");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        let other = root.join("images").join("unrelated.png");
        std::fs::create_dir_all(root.join("images")).unwrap();
        std::fs::write(&other, b"keep me").unwrap();
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        paste_without_waiting(&view, cx);
        // Step the scheduler one task at a time until the background save has
        // written this job's PNG. Delivery to the view is a later task, so it
        // has not run yet when the file first appears.
        let mut steps = 0;
        while pasted_files(&root).len() < 2 {
            assert!(steps < 10_000, "the background save never wrote its file");
            cx.executor().tick();
            steps += 1;
        }
        assert_eq!(
            view.read_with(cx, |view, _| view.editor().document().full_text()),
            "body\n",
            "the save finished before the outcome was delivered"
        );
        // The save succeeded; now the view goes away, so `view.update` fails.
        cx.update(|window, _| window.remove_window());
        drop(view);
        cx.run_until_parked();
        assert_eq!(pasted_files(&root), vec![other.clone()]);
        assert_eq!(std::fs::read(&other).unwrap(), b"keep me");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_published_image_survives_the_view_release(cx: &mut gpui::TestAppContext) {
        let root = temp_root("published-release");
        let (view, cx) = open_file_view(cx, &root, "body\n");
        let other = root.join("images").join("unrelated.png");
        std::fs::create_dir_all(root.join("images")).unwrap();
        std::fs::write(&other, b"keep me").unwrap();
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(1, 1, [0, 0, 0, 255])));
        paste_without_waiting(&view, cx);
        cx.run_until_parked();
        assert_eq!(pasted_files(&root).len(), 2);
        cx.simulate_keystrokes("secondary-z");
        cx.run_until_parked();
        cx.update(|window, _| window.remove_window());
        drop(view);
        cx.run_until_parked();
        let files = pasted_files(&root);
        assert_eq!(files.len(), 2, "{files:?}");
        assert!(files.contains(&other));
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
    fn a_draft_first_saved_into_its_target_directory_keeps_the_pasted_image(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("draft-first-save");
        let target = root.join("folder");
        std::fs::create_dir_all(&target).unwrap();
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        view.update(cx, |view, _| {
            let id = view.sessions.active_id();
            view.work_folder_drafts.insert(
                id,
                WorkFolderDraft {
                    draft_id: DraftId::generate(),
                    target_directory: target.clone(),
                },
            );
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| window.focus(&view.focus_handle, cx));
        });
        cx.write_to_clipboard(image_item(None, png(2, 2, [5, 5, 5, 255])));
        paste_without_waiting(&view, cx);
        // The first save lands in the same directory before the paste result
        // is delivered; the save retires the draft entry.
        save_as(&view, cx, target.join("note.md"));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.work_folder_drafts.is_empty());
        });
        let saved = std::fs::read_to_string(target.join("note.md")).unwrap();
        let name = reference_in(&text_of(&view, cx));
        assert!(text_of(&view, cx).contains(&format!("images/{name}")));
        assert!(target.join("images").join(&name).is_file());
        assert_eq!(pasted_files(&target).len(), 1);
        let _ = saved;
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
    fn an_unrelated_save_success_does_not_resume_a_paste_waiting_for_its_own_save_as(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("unrelated-save-while-paste-waits");
        let unrelated = root.join("unrelated");
        std::fs::create_dir_all(&unrelated).unwrap();
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| window.focus(&view.focus_handle, cx));
        });
        cx.write_to_clipboard(image_item(None, png(2, 2, [8, 8, 8, 255])));
        // The standalone untitled document asks where to save; that Save As
        // prompt stays unanswered for the rest of the test.
        paste_without_waiting(&view, cx);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.image_paste.pending_save.is_some());
        });
        // A different save request of the same document succeeds meanwhile.
        let unrelated_path = unrelated.join("other.md");
        save_as(&view, cx, unrelated_path.clone());
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(&unrelated_path).unwrap(), "");
        // Only the paste's own save request may resume it: the body, the
        // destination and the image files stay as the unrelated save left them.
        assert_eq!(text_of(&view, cx), "");
        assert!(pasted_files(&unrelated).is_empty());
        assert!(pasted_files(&root).is_empty());
        view.read_with(cx, |view, _| {
            let id = view.sessions.active_id();
            assert_eq!(
                view.sessions.get(id).and_then(DocumentSession::path),
                Some(unrelated_path.as_path())
            );
            assert!(view.image_paste.pending_save.is_some());
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn an_earlier_write_to_the_same_destination_does_not_resume_a_paste_before_its_own_save_as(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("same-destination-save-while-paste-waits");
        let destination = root.join("note.md");
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| window.focus(&view.focus_handle, cx));
        });
        cx.write_to_clipboard(image_item(None, png(2, 2, [9, 9, 9, 255])));
        // The standalone untitled document asks where to save; the paste waits
        // for its own Save As answer.
        paste_without_waiting(&view, cx);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.image_paste.pending_save.is_some());
        });
        // An earlier Save As to the destination starts writing, and the paste's
        // own prompt then picks the same destination while that write is still
        // in flight, so its request queues behind the earlier one.
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            let instance = view.document_instance(id);
            view.save_as_to(id, destination.clone(), false, cx);
            assert!(view.sessions.active().save_in_flight());
            view.handle_save_as_response(
                id,
                instance,
                crate::view::SaveAsResponse::Chosen(destination.clone()),
                cx,
            );
            assert!(view.image_paste.pending_save.is_some());
        });
        // Step the scheduler one task at a time. The paste may only resume once
        // the write of its own request has landed, i.e. with no save in flight;
        // the earlier write's completion must not resume it while the queued
        // write is running, and nothing is created before that.
        let mut steps = 0;
        loop {
            let resumed = view.read_with(cx, |view, _| view.image_paste.pending_save.is_none());
            if resumed {
                break;
            }
            assert_eq!(text_of(&view, cx), "");
            assert!(steps < 10_000, "the paste never resumed");
            assert!(pasted_files(&root).is_empty(), "no image exists before the resume");
            cx.executor().tick();
            steps += 1;
        }
        view.read_with(cx, |view, _| {
            assert!(
                !view.sessions.active().save_in_flight(),
                "the paste resumed while its own save request was still running"
            );
        });
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "");
        cx.run_until_parked();
        let text = text_of(&view, cx);
        let name = reference_in(&text);
        assert!(root.join("images").join(&name).is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn an_image_paste_finishing_during_a_save_as_to_another_directory_is_aborted(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("paste-during-save-as");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&to).unwrap();
        let (view, cx) = open_file_view(cx, &from, "body\n");
        select(&view, cx, 4, 4);
        cx.write_to_clipboard(image_item(None, png(2, 2, [4, 4, 4, 255])));
        paste_without_waiting(&view, cx);
        // The Save As to another directory is writing when the background
        // save of the pasted image is confirmed.
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.save_as_to(id, to.join("note.md"), false, cx);
            assert!(view.sessions.active().save_in_flight());
            assert!(view.sessions.active().writes_outside_directory(&from));
        });
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(to.join("note.md")).unwrap(), "body\n");
        assert_eq!(text_of(&view, cx), "body\n");
        assert!(pasted_files(&from).is_empty(), "the unpublished image is removed");
        assert!(pasted_files(&to).is_empty());
        std::fs::remove_dir_all(root).unwrap();
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
    fn a_save_as_blocked_by_a_folder_move_without_a_rewrite_removes_its_own_copy(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-blocked-no-rewrite");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(to.join("images")).unwrap();
        let name = "hane-paste-1-0123abcd.png";
        std::fs::write(from.join("images").join(name), b"png-bytes").unwrap();
        // An unrelated image in the destination is not ours to remove.
        std::fs::write(to.join("images").join("other.png"), b"other").unwrap();
        let body = format!("![画像](images/{name})\n");
        let (view, cx) = open_file_view(cx, &from, &body);
        view.update(cx, |view, _| {
            view.folder_trash_in_flight = Some(to.clone());
        });
        save_as(&view, cx, to.join("note.md"));
        assert!(!to.join("note.md").exists(), "no document is written");
        assert!(!to.join("images").join(name).exists(), "the unpublished copy is removed");
        assert!(from.join("images").join(name).is_file(), "the original image is kept");
        assert_eq!(
            std::fs::read(to.join("images").join("other.png")).unwrap(),
            b"other"
        );
        assert_eq!(text_of(&view, cx), body);
        view.read_with(cx, |view, _| {
            assert!(view.image_paste.save_as_copies.is_empty());
            assert!(view.tab_close_after_save.is_empty());
            assert!(!view.sessions.active().save_in_flight());
        });
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

    const NAME_ONE: &str = "hane-paste-1-0123abcd.png";
    const NAME_TWO: &str = "hane-paste-2-4567ef01.png";

    fn edit_view(view: &TestView, cx: &mut TestCx, command: EditorCommand<'static>) {
        view.update(cx, |view, cx| {
            view.editor_mut().dispatch(command).unwrap();
            view.after_input(cx);
        });
    }

    /// Every reference of `text` names an image of `directory` holding the
    /// bytes of the image that reference started as.
    fn assert_references_resolve(text: &str, directory: &Path, expected: &[&[u8]]) {
        let names: Vec<String> = clip::generated_references(text)
            .into_iter()
            .map(|reference| reference.file_name)
            .collect();
        assert_eq!(names.len(), expected.len(), "{text:?}");
        for (name, bytes) in names.iter().zip(expected) {
            assert_eq!(
                std::fs::read(directory.join("images").join(name)).unwrap(),
                *bytes,
                "{text:?}"
            );
        }
    }

    #[gpui::test]
    fn a_failed_body_write_after_the_copy_keeps_the_document_and_removes_the_copies(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-write-fail");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(to.join("images")).unwrap();
        std::fs::write(from.join("images").join(NAME_ONE), b"mine").unwrap();
        std::fs::write(to.join("images").join(NAME_ONE), b"someone else's").unwrap();
        // The copy succeeds; writing the body onto a directory cannot.
        std::fs::create_dir(to.join("note.md")).unwrap();
        let body = format!("![画像](images/{NAME_ONE})\n");
        let (view, cx) = open_file_view(cx, &from, &body);
        select(&view, cx, 0, 0);
        edit_view(&view, cx, EditorCommand::Insert("x"));
        let edited = text_of(&view, cx);
        let selection = view.read_with(cx, |view, _| view.editor().selection());
        let (revision, instance) = view.read_with(cx, |view, _| {
            (
                view.sessions.active().revision(),
                view.document_instance(view.sessions.active_id()),
            )
        });
        save_as(&view, cx, to.join("note.md"));
        assert_eq!(text_of(&view, cx), edited);
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), selection);
            assert_eq!(view.sessions.active().revision(), revision);
            assert_eq!(view.document_instance(view.sessions.active_id()), instance);
            assert!(view.sessions.active().is_dirty());
            assert!(view.sessions.active().path().is_some_and(|path| path.starts_with(&from)));
        });
        assert_eq!(
            pasted_files(&to),
            vec![to.join("images").join(NAME_ONE)],
            "the copy nothing refers to is removed"
        );
        assert_eq!(std::fs::read(to.join("images").join(NAME_ONE)).unwrap(), b"someone else's");
        assert_eq!(std::fs::read(from.join("images").join(NAME_ONE)).unwrap(), b"mine");
        // The history is still the user's own.
        edit_view(&view, cx, EditorCommand::Undo);
        assert_eq!(text_of(&view, cx), body);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn conflicting_images_stay_correct_through_undo_and_redo_after_save_as(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-history");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(to.join("images")).unwrap();
        std::fs::write(from.join("images").join(NAME_ONE), b"one").unwrap();
        std::fs::write(from.join("images").join(NAME_TWO), b"two").unwrap();
        std::fs::write(to.join("images").join(NAME_ONE), b"other-1").unwrap();
        std::fs::write(to.join("images").join(NAME_TWO), b"other-2").unwrap();
        let body = format!(
            "前\n\n![画像](images/{NAME_ONE})\n\n![画像](images/{NAME_TWO})\n\n後\n"
        );
        let (view, cx) = open_file_view(cx, &from, &body);
        select(&view, cx, 0, 0);
        edit_view(&view, cx, EditorCommand::Insert("あ"));
        let end = body.len() + "あ".len();
        select(&view, cx, end, end);
        edit_view(&view, cx, EditorCommand::Insert("い"));
        let expected: [&[u8]; 2] = [b"one", b"two"];

        save_as(&view, cx, to.join("note.md"));

        let saved = std::fs::read_to_string(to.join("note.md")).unwrap();
        assert_eq!(text_of(&view, cx), saved);
        assert!(saved.starts_with('あ') && saved.ends_with("い"));
        assert_references_resolve(&saved, &to, &expected);
        assert!(!saved.contains(NAME_ONE) && !saved.contains(NAME_TWO));
        assert!(!view.read_with(cx, |view, _| view.sessions.active().is_dirty()));
        let mut states = vec![saved];
        for _ in 0..2 {
            edit_view(&view, cx, EditorCommand::Undo);
            states.push(text_of(&view, cx));
        }
        for _ in 0..2 {
            edit_view(&view, cx, EditorCommand::Redo);
            states.push(text_of(&view, cx));
        }
        assert!(view.read_with(cx, |view, _| !view.editor().can_redo()));
        assert!(states[1].ends_with("後\n") && states[2].starts_with('前'));
        for state in &states {
            assert_references_resolve(state, &to, &expected);
        }
        assert_eq!(states[0], states[4]);
        assert_eq!(std::fs::read(to.join("images").join(NAME_ONE)).unwrap(), b"other-1");
        assert_eq!(std::fs::read(to.join("images").join(NAME_TWO)).unwrap(), b"other-2");
        assert_eq!(std::fs::read(from.join("images").join(NAME_ONE)).unwrap(), b"one");
        assert_eq!(std::fs::read(from.join("images").join(NAME_TWO)).unwrap(), b"two");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn save_as_copies_generated_images_referenced_only_by_undo_redo_history(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-history-only");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(to.join("images")).unwrap();
        std::fs::write(from.join("images").join(NAME_ONE), b"one").unwrap();
        std::fs::write(from.join("images").join(NAME_TWO), b"two").unwrap();
        // Only the first name is taken in the target directory.
        std::fs::write(to.join("images").join(NAME_ONE), b"other-1").unwrap();
        let body = "前\n";
        let (view, cx) = open_file_view(cx, &from, body);
        let end = body.len();
        select(&view, cx, end, end);
        const PASTED: &str =
            "![画像](images/hane-paste-1-0123abcd.png)\n![画像](images/hane-paste-2-4567ef01.png)\n";
        edit_view(&view, cx, EditorCommand::Insert(PASTED));
        edit_view(&view, cx, EditorCommand::Undo);
        assert_eq!(text_of(&view, cx), body);
        assert!(view.read_with(cx, |view, _| view.editor().can_redo()));

        save_as(&view, cx, to.join("note.md"));

        let saved = std::fs::read_to_string(to.join("note.md")).unwrap();
        assert_eq!(saved, body, "the saved body refers to no image");
        assert_eq!(text_of(&view, cx), body);
        edit_view(&view, cx, EditorCommand::Redo);
        let restored = text_of(&view, cx);
        let expected: [&[u8]; 2] = [b"one", b"two"];
        assert_references_resolve(&restored, &to, &expected);
        assert!(!restored.contains(NAME_ONE), "the conflicting name is renamed");
        assert!(restored.contains(NAME_TWO), "the free name is kept");
        assert_eq!(std::fs::read(to.join("images").join(NAME_ONE)).unwrap(), b"other-1");
        assert_eq!(std::fs::read(from.join("images").join(NAME_ONE)).unwrap(), b"one");
        assert_eq!(std::fs::read(from.join("images").join(NAME_TWO)).unwrap(), b"two");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn save_as_leaves_code_examples_untouched_next_to_a_conflicting_real_image(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-examples");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(to.join("images")).unwrap();
        std::fs::write(from.join("images").join(NAME_ONE), b"one").unwrap();
        std::fs::write(to.join("images").join(NAME_ONE), b"other-1").unwrap();
        let fenced = format!("```md\n![例](images/{NAME_ONE})\n```\n");
        let inline = format!("`![例](images/{NAME_ONE})`\n");
        let body = format!("前\n\n{fenced}\n{inline}\n![画像](images/{NAME_ONE})\n\n後\n");
        let (view, cx) = open_file_view(cx, &from, &body);
        select(&view, cx, 0, 0);
        edit_view(&view, cx, EditorCommand::Insert("あ"));
        let expected: [&[u8]; 1] = [b"one"];
        let assert_examples_intact = |text: &str| {
            assert!(text.contains(&fenced), "{text:?}");
            assert!(text.contains(&inline), "{text:?}");
            assert_references_resolve(text, &to, &expected);
        };

        save_as(&view, cx, to.join("note.md"));

        let saved = std::fs::read_to_string(to.join("note.md")).unwrap();
        assert_eq!(text_of(&view, cx), saved);
        assert_examples_intact(&saved);
        let new_name = reference_in(&saved);
        assert_ne!(new_name, NAME_ONE);
        assert!(saved.ends_with(&format!("![画像](images/{new_name})\n\n後\n")));
        edit_view(&view, cx, EditorCommand::Undo);
        let undone = text_of(&view, cx);
        edit_view(&view, cx, EditorCommand::Redo);
        let redone = text_of(&view, cx);
        for state in [&saved, &undone, &redone] {
            assert_examples_intact(state);
        }
        assert_eq!(saved, redone);
        assert_eq!(std::fs::read(to.join("images").join(NAME_ONE)).unwrap(), b"other-1");
        assert_eq!(std::fs::read(from.join("images").join(NAME_ONE)).unwrap(), b"one");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_document_with_only_code_examples_is_saved_as_is_without_copies(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-only-examples");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("images").join(NAME_ONE), b"one").unwrap();
        let body = format!("```\n![例](images/{NAME_ONE})\n```\n\n`![例](images/{NAME_ONE})`\n");
        let (view, cx) = open_file_view(cx, &from, &body);
        save_as(&view, cx, to.join("note.md"));
        assert_eq!(std::fs::read_to_string(to.join("note.md")).unwrap(), body);
        assert!(!to.join("images").exists(), "no needless copy");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_background_tab_save_as_rewrites_only_its_own_document(cx: &mut gpui::TestAppContext) {
        let root = temp_root("save-as-background");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(to.join("images")).unwrap();
        std::fs::write(from.join("images").join(NAME_ONE), b"mine").unwrap();
        std::fs::write(to.join("images").join(NAME_ONE), b"someone else's").unwrap();
        let (view, cx) = open_file_view(cx, &from, &format!("![画像](images/{NAME_ONE})\n"));
        let background = view.read_with(cx, |view, _| view.sessions.active_id());
        view.update(cx, |view, cx| {
            view.sessions.open_untitled("other\n", "Other");
            view.on_document_replaced();
            cx.notify();
        });
        select(&view, cx, 2, 2);
        let selection = view.read_with(cx, |view, _| view.editor().selection());

        view.update(cx, |view, cx| {
            view.save_as_to(background, to.join("note.md"), false, cx);
        });
        cx.run_until_parked();

        let saved = std::fs::read_to_string(to.join("note.md")).unwrap();
        assert_references_resolve(&saved, &to, &[b"mine"]);
        view.read_with(cx, |view, _| {
            let document = view.sessions.get(background).unwrap();
            assert_eq!(document.editor().document().full_text(), saved);
            assert!(!document.is_dirty());
            assert_ne!(view.sessions.active_id(), background);
            assert_eq!(view.editor().document().full_text(), "other\n");
            assert_eq!(view.editor().selection(), selection);
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_save_already_running_abandons_the_save_as_and_its_copies(cx: &mut gpui::TestAppContext) {
        let root = temp_root("save-as-busy");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(to.join("images")).unwrap();
        std::fs::write(from.join("images").join(NAME_ONE), b"mine").unwrap();
        std::fs::write(to.join("images").join(NAME_ONE), b"someone else's").unwrap();
        let body = format!("![画像](images/{NAME_ONE})\n");
        let (view, cx) = open_file_view(cx, &from, &body);
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            // This write is never completed, so it keeps the save slot.
            let decision = view
                .sessions
                .active_mut()
                .request_save(hane_session::SaveIntent::Current);
            assert!(matches!(decision, hane_session::SaveDecision::Write(_)));
            view.save_as_to(id, to.join("note.md"), false, cx);
        });
        cx.run_until_parked();
        assert!(!to.join("note.md").exists());
        assert_eq!(pasted_files(&to), vec![to.join("images").join(NAME_ONE)]);
        assert_eq!(text_of(&view, cx), body);
        assert!(status_of(&view, cx).unwrap().contains("別の保存"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn an_abandoned_save_as_keeps_a_copy_another_document_refers_to(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("save-as-shared-copy");
        let (from, to) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(from.join("images")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("images").join(NAME_ONE), b"png").unwrap();
        std::fs::write(to.join("other.md"), format!("![x](images/{NAME_ONE})\n")).unwrap();
        let other = OsFileService.load(&to.join("other.md")).unwrap();
        let (view, cx) = open_file_view(cx, &from, &format!("![画像](images/{NAME_ONE})\n"));
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            view.save_as_to(id, to.join("note.md"), false, cx);
            // The document changes during the copy, so the Save As is
            // abandoned; the other document in the target directory is open.
            view.editor_mut().insert_text("x").unwrap();
            view.after_input(cx);
            view.sessions.apply_open(None, other);
            view.on_document_replaced();
            cx.notify();
        });
        cx.run_until_parked();
        assert!(!to.join("note.md").exists());
        assert_eq!(std::fs::read(to.join("images").join(NAME_ONE)).unwrap(), b"png");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_snapshot_prefers_a_raster_and_keeps_the_text() {
        let snapshot = ClipboardSnapshot::from_item(&image_item(Some("t"), png(1, 1, [0; 4])));
        assert_eq!(snapshot.text(), Some("t"));
        assert!(matches!(snapshot.images, ClipboardImages::One(_)));
        assert!(snapshot.has_paste_content());
        let empty = ClipboardSnapshot::from_item(&ClipboardItem::new_string(String::new()));
        assert!(!empty.has_paste_content());
    }

    fn image_of(bytes: Vec<u8>, format: gpui::ImageFormat) -> gpui::Image {
        let id = gpui::hash(&bytes);
        gpui::Image { format, bytes, id }
    }

    fn native_read(
        text: Option<&str>,
        with_paths: bool,
        images: Vec<Result<gpui::Image, ClipboardReadError>>,
    ) -> NativeRead {
        let mut entries = Vec::new();
        if with_paths {
            entries.push(gpui::ClipboardEntry::ExternalPaths(
                gpui::ExternalPaths::default(),
            ));
        }
        if let Some(text) = text {
            entries.push(gpui::ClipboardEntry::String(gpui::ClipboardString::new(
                text.to_owned(),
            )));
        }
        NativeRead {
            item: ClipboardItem { entries },
            html: None,
            html_problem: false,
            images: images
                .into_iter()
                .map(|image| image.map_err(ImageReadFailure::from))
                .collect(),
        }
    }

    fn paste_native(
        view: &TestView,
        cx: &mut TestCx,
        read: Result<Option<NativeRead>, ClipboardReadError>,
    ) {
        view.update(cx, |view, cx| {
            let snapshot = ClipboardSnapshot::from_native(read);
            view.paste_image_from_snapshot(&snapshot, cx);
        });
        cx.run_until_parked();
    }

    // A failed or busy clipboard is not an empty one: it is explained, the
    // body, the selection and the files stay as they were, and no text is
    // pasted in its place.
    #[gpui::test]
    fn a_failed_clipboard_read_is_reported_and_changes_nothing(cx: &mut gpui::TestAppContext) {
        for (error, reason) in [
            (ClipboardReadError::Busy, "使用中"),
            (ClipboardReadError::ChangedDuringRead, "変更されました"),
            (ClipboardReadError::InvalidFormat, "正しくありません"),
            (ClipboardReadError::TooLarge, "大きすぎ"),
        ] {
            let root = temp_root("read-error");
            let (view, cx) = open_file_view(cx, &root, "keep this\n");
            select(&view, cx, 0, 4);
            paste_native(&view, cx, Err(error));
            assert_eq!(text_of(&view, cx), "keep this\n", "{error:?}");
            assert!(status_of(&view, cx).unwrap().contains(reason), "{error:?}");
            let selection = view.read_with(cx, |view, _| view.editor().selection());
            assert_eq!(selection.range(), SourceRange::new(0, 4));
            assert!(!root.join("images").exists());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[gpui::test]
    fn a_really_empty_clipboard_does_nothing_and_says_nothing(cx: &mut gpui::TestAppContext) {
        let root = temp_root("really-empty");
        let (view, cx) = open_file_view(cx, &root, "keep\n");
        select(&view, cx, 0, 4);
        let before = status_of(&view, cx);
        paste_native(&view, cx, Ok(None));
        assert_eq!(text_of(&view, cx), "keep\n");
        assert_eq!(status_of(&view, cx), before);
        assert!(!root.join("images").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    // An image the platform could not take in is explained; the text beside
    // it is not pasted instead.
    #[gpui::test]
    fn an_unreadable_image_never_falls_back_to_its_text(cx: &mut gpui::TestAppContext) {
        for (error, reason) in [
            (ClipboardReadError::InvalidFormat, "正しくありません"),
            (ClipboardReadError::TooLarge, "大きすぎ"),
        ] {
            let root = temp_root("unreadable");
            let (view, cx) = open_file_view(cx, &root, "keep this\n");
            select(&view, cx, 0, 4);
            paste_native(
                &view,
                cx,
                Ok(Some(native_read(Some("fallback text"), false, vec![Err(error)]))),
            );
            assert_eq!(text_of(&view, cx), "keep this\n", "{error:?}");
            assert!(status_of(&view, cx).unwrap().contains(reason), "{error:?}");
            let selection = view.read_with(cx, |view, _| view.editor().selection());
            assert_eq!(selection.range(), SourceRange::new(0, 4));
            assert!(!root.join("images").exists());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    // Independent images are not cut down to the first one nor merged: the
    // paste is refused before any file exists.
    #[gpui::test]
    fn several_independent_images_are_refused_with_the_body_unchanged(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("several");
        let (view, cx) = open_file_view(cx, &root, "keep\n");
        select(&view, cx, 4, 4);
        let first = image_of(png(1, 1, [1, 1, 1, 255]), gpui::ImageFormat::Png);
        let second = image_of(png(2, 2, [2, 2, 2, 255]), gpui::ImageFormat::Png);
        paste_native(
            &view,
            cx,
            Ok(Some(native_read(Some("text"), false, vec![Ok(first), Ok(second)]))),
        );
        assert_eq!(text_of(&view, cx), "keep\n");
        assert!(status_of(&view, cx).unwrap().contains("2件"));
        assert!(!root.join("images").exists());

        // The same through a GPUI item with two image entries.
        let mut item = image_item(None, png(1, 1, [1, 1, 1, 255]));
        item.entries.push(gpui::ClipboardEntry::Image(image_of(
            png(2, 2, [2, 2, 2, 255]),
            gpui::ImageFormat::Png,
        )));
        view.update(cx, |view, cx| {
            let snapshot = ClipboardSnapshot::from_item(&item);
            view.paste_image_from_snapshot(&snapshot, cx);
        });
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "keep\n");
        assert!(!root.join("images").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    // One image item (the platform already picked its preferred format) beside
    // an SVG-only item and the file paths and plain text of the same
    // generation: the real image is pasted, and only the body prefers it.
    #[gpui::test]
    fn paths_plain_and_an_image_paste_the_image_and_keep_the_others_out(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = temp_root("paths-plain-image");
        let (view, cx) = open_file_view(cx, &root, "hello\n");
        select(&view, cx, 6, 6);
        let image = image_of(png(2, 1, [3, 4, 5, 255]), gpui::ImageFormat::Png);
        let svg = image_of(b"<svg/>".to_vec(), gpui::ImageFormat::Svg);
        let read = native_read(Some("/a/b.png"), true, vec![Ok(image), Ok(svg)]);
        paste_native(&view, cx, Ok(Some(read)));
        let text = text_of(&view, cx);
        let name = reference_in(&text);
        assert_eq!(text, format!("hello\n\n![画像](images/{name})\n"));
        assert!(!text.contains("/a/b.png"));
        assert_eq!(pasted_files(&root).len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_snapshot_keeps_the_text_of_a_paths_and_plain_clipboard_without_an_image() {
        let read = native_read(Some("/a/b.txt"), true, Vec::new());
        let snapshot = ClipboardSnapshot::from_native(Ok(Some(read)));
        assert!(matches!(snapshot.images, ClipboardImages::Empty));
        assert_eq!(snapshot.text(), Some("/a/b.txt"));
        assert!(snapshot.read_error.is_none());
    }

    #[test]
    fn the_snapshot_tells_an_empty_clipboard_from_a_failed_read() {
        let empty = ClipboardSnapshot::from_native(Ok(None));
        assert!(!empty.has_paste_content());
        assert!(empty.read_error.is_none());
        for error in [
            ClipboardReadError::Busy,
            ClipboardReadError::ChangedDuringRead,
        ] {
            let failed = ClipboardSnapshot::from_native(Err(error));
            assert_eq!(failed.read_error, Some(error));
            assert!(failed.text().is_none());
        }
    }

    #[test]
    fn the_snapshot_counts_independent_raster_items_and_ignores_svg() {
        let png_image = || image_of(png(1, 1, [0; 4]), gpui::ImageFormat::Png);
        let svg = || image_of(b"<svg/>".to_vec(), gpui::ImageFormat::Svg);
        let of = |images| {
            ClipboardSnapshot::from_native(Ok(Some(native_read(None, false, images))))
        };
        assert!(matches!(of(vec![Ok(svg())]).images, ClipboardImages::Empty));
        assert!(matches!(
            of(vec![Ok(png_image()), Ok(svg())]).images,
            ClipboardImages::One(_)
        ));
        assert!(matches!(
            of(vec![Ok(png_image()), Ok(png_image()), Ok(png_image())]).images,
            ClipboardImages::Several(3)
        ));
        assert!(matches!(
            of(vec![Err(ClipboardReadError::TooLarge)]).images,
            ClipboardImages::Unreadable(ClipboardReadError::TooLarge)
        ));
        assert!(matches!(
            of(vec![Ok(png_image()), Err(ClipboardReadError::InvalidFormat)]).images,
            ClipboardImages::Several(2)
        ));
    }

    #[test]
    fn an_svg_read_error_is_no_raster_candidate_but_a_raster_error_is() {
        let svg_failure = |error| ImageReadFailure {
            error,
            format: Some(gpui::ImageFormat::Svg),
        };
        let png_failure = |error| ImageReadFailure {
            error,
            format: Some(gpui::ImageFormat::Png),
        };
        let of = |images| {
            let mut read = native_read(Some("text"), false, Vec::new());
            read.images = images;
            ClipboardSnapshot::from_native(Ok(Some(read)))
        };
        for error in [ClipboardReadError::InvalidFormat, ClipboardReadError::TooLarge] {
            let snapshot = of(vec![Err(svg_failure(error))]);
            assert!(matches!(snapshot.images, ClipboardImages::Empty));
            assert_eq!(snapshot.text(), Some("text"));
            assert!(matches!(
                of(vec![Err(png_failure(error))]).images,
                ClipboardImages::Unreadable(e) if e == error
            ));
        }
        let png_ok = image_of(png(1, 1, [0; 4]), gpui::ImageFormat::Png);
        assert!(matches!(
            of(vec![Ok(png_ok), Err(svg_failure(ClipboardReadError::TooLarge))]).images,
            ClipboardImages::One(_)
        ));
    }
}
