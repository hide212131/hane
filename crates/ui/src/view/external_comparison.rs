//! The read-only comparison of the disk version and the version being edited
//! (Issue #653, "差分を確認").
//!
//! Opening it only copies two texts that are already in memory: the disk
//! version the session observed in the background, and the editing buffer.
//! Nothing is read from disk and no hash is computed here. Opening, closing
//! and re-comparing never touch the text, the dirty state, the undo / redo
//! history, the draft, the saved baseline, a resolution approval or the file.
//!
//! The comparison is bound to the document instance (generation), the editing
//! revision and the fingerprint of the observed disk version. When any of them
//! no longer matches, the window says the content was updated and asks for a
//! new comparison; a stale comparison is never treated as current, and the
//! reload / overwrite / save-as choices refuse to run from it.

use super::*;
use hane_session::ContentFingerprint;
use std::ops::Range;

/// Lines of each version that are drawn. A larger document is cut with a
/// notice, so the cost of a frame does not grow with the document.
const MAX_COMPARISON_LINES: usize = 2_000;

/// Fixed snapshots of the two versions, with what they were taken against.
pub(super) struct ExternalComparison {
    id: SessionId,
    instance: DocumentInstance,
    revision: Revision,
    observed: ContentFingerprint,
    disk: String,
    mine: String,
}

/// Whether a comparison still describes what the document and the disk are.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ComparisonFreshness {
    Current,
    /// The edited text or the observed disk version changed since it was taken.
    Updated,
    /// The document it was taken from was replaced or closed.
    Gone,
}

/// The lines (of each side) that differ: everything between the lines the two
/// sides share at the start and, when `from_end` is set, at the end.
fn changed_span(left: &[&str], right: &[&str], from_end: bool) -> (Range<usize>, Range<usize>) {
    let prefix = left
        .iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = if from_end {
        let room = left.len().min(right.len()) - prefix;
        left[prefix..]
            .iter()
            .rev()
            .zip(right[prefix..].iter().rev())
            .take(room)
            .take_while(|(left, right)| left == right)
            .count()
    } else {
        0
    };
    (prefix..left.len() - suffix, prefix..right.len() - suffix)
}

/// The first `MAX_COMPARISON_LINES` lines of `text`, and whether more follow.
fn displayed_lines(text: &str) -> (Vec<&str>, bool) {
    let mut lines = text.split('\n');
    let shown: Vec<&str> = lines
        .by_ref()
        .take(MAX_COMPARISON_LINES)
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    (shown, lines.next().is_some())
}

impl EditorView {
    /// Copies the two versions of `id`, or `None` when the session has no
    /// observed disk version to show (never an empty or older stand-in).
    fn external_comparison_snapshot(&self, id: SessionId) -> Option<ExternalComparison> {
        let instance = self.document_instance(id)?;
        let session = self.sessions.get(id)?;
        let observed = session.observed_fingerprint()?;
        let disk = session.observed_document()?.full_text();
        Some(ExternalComparison {
            id,
            instance,
            revision: session.revision(),
            observed,
            disk,
            mine: session.editor().document().full_text(),
        })
    }

    /// Opens the comparison for the active document.
    pub(super) fn open_external_comparison(&mut self, cx: &mut Context<Self>) {
        let id = self.sessions.active_id();
        match self.external_comparison_snapshot(id) {
            Some(comparison) => self.external_comparison = Some(comparison),
            None => {
                self.external_comparison = None;
                self.status = Some(
                    "比較できるディスクの内容がありません。もう一度確認してください".to_owned(),
                );
            }
        }
        cx.notify();
    }

    /// While the comparison is shown the body is read-only: keyboard actions
    /// and OS text input must not reach the document behind it.
    pub(crate) fn external_comparison_blocks_body(&self) -> bool {
        self.external_comparison.is_some()
    }

    pub(super) fn close_external_comparison(&mut self, cx: &mut Context<Self>) {
        if self.external_comparison.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn external_comparison_freshness(&self) -> Option<ComparisonFreshness> {
        let comparison = self.external_comparison.as_ref()?;
        if self.document_instance(comparison.id) != Some(comparison.instance) {
            return Some(ComparisonFreshness::Gone);
        }
        let session = self.sessions.get(comparison.id)?;
        let current = session.revision() == comparison.revision
            && session.observed_fingerprint() == Some(comparison.observed);
        Some(if current {
            ComparisonFreshness::Current
        } else {
            ComparisonFreshness::Updated
        })
    }

    /// Drops a comparison that belongs to a replaced document or to a tab that
    /// is not the active one, so it is never drawn over another document.
    pub(super) fn prune_external_comparison(&mut self) {
        let drop_it = self.external_comparison.as_ref().is_some_and(|comparison| {
            comparison.id != self.sessions.active_id()
                || matches!(
                    self.external_comparison_freshness(),
                    None | Some(ComparisonFreshness::Gone)
                )
        });
        if drop_it {
            self.external_comparison = None;
        }
    }

    /// Whether an open comparison is out of date. Reload, overwrite and
    /// save-as do not run while this holds.
    pub(super) fn external_comparison_is_stale(&self) -> bool {
        self.external_comparison_freshness()
            .is_some_and(|freshness| freshness != ComparisonFreshness::Current)
    }

    /// Compares again with the newest observation the session has, and asks
    /// the existing background check to look at the disk once more. The new
    /// check, if it finds another version, marks this comparison updated again.
    pub(super) fn refresh_external_comparison(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.external_comparison.as_ref().map(|c| c.id) else {
            return;
        };
        if self.external_comparison_freshness() == Some(ComparisonFreshness::Gone) {
            self.external_comparison = None;
            cx.notify();
            return;
        }
        match self.external_comparison_snapshot(id) {
            Some(comparison) => self.external_comparison = Some(comparison),
            None => {
                self.external_comparison = None;
                self.status = Some(
                    "ディスクの内容が変わったため比較を閉じました。もう一度確認してください"
                        .to_owned(),
                );
            }
        }
        self.start_external_check(id, cx);
        cx.notify();
    }

    fn comparison_column(
        &self,
        key: &'static str,
        title: &'static str,
        text: &str,
        other: &str,
        is_left: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let (lines, cut) = displayed_lines(text);
        let (other_lines, other_cut) = displayed_lines(other);
        let (left, right) = if is_left {
            (&lines, &other_lines)
        } else {
            (&other_lines, &lines)
        };
        let (left_span, right_span) = changed_span(left, right, !(cut || other_cut));
        let span = if is_left { left_span } else { right_span };
        div()
            .id(key)
            .debug_selector(move || key.to_owned())
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_y_scroll()
            .child(div().font_weight(gpui::FontWeight::BOLD).child(title))
            .children(lines.into_iter().enumerate().map(|(index, line)| {
                div()
                    .w_full()
                    .whitespace_normal()
                    .when(span.contains(&index), |element| {
                        element.bg(rgba(0xffa5_0040))
                    })
                    .child(if line.is_empty() {
                        " ".to_owned()
                    } else {
                        line.to_owned()
                    })
            }))
            .when(cut, |element| {
                element.child(div().child(format!(
                    "（先頭の{MAX_COMPARISON_LINES}行だけを表示しています）"
                )))
            })
    }

    /// The comparison window, when one is open for the active document.
    pub(super) fn external_comparison_overlay(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let comparison = self.external_comparison.as_ref()?;
        if comparison.id != self.sessions.active_id() {
            return None;
        }
        let updated = match self.external_comparison_freshness()? {
            ComparisonFreshness::Gone => return None,
            ComparisonFreshness::Updated => true,
            ComparisonFreshness::Current => false,
        };
        let columns = h_flex()
            .flex_1()
            .min_h(px(0.0))
            .gap_2()
            .child(self.comparison_column(
                "external-comparison-disk",
                "ディスク上の版（読み取り専用）",
                &comparison.disk,
                &comparison.mine,
                true,
            ))
            .child(self.comparison_column(
                "external-comparison-mine",
                "編集中の版（読み取り専用）",
                &comparison.mine,
                &comparison.disk,
                false,
            ));
        let close = self.file_tab_context_menu_item(
            "external-comparison-close",
            "閉じる",
            true,
            cx,
            |view, _, _, cx| view.close_external_comparison(cx),
        );
        let refresh = self.file_tab_context_menu_item(
            "external-comparison-refresh",
            "最新の内容で再比較",
            true,
            cx,
            |view, _, _, cx| view.refresh_external_comparison(cx),
        );
        Some(
            div()
                .id("external-comparison-overlay")
                .debug_selector(|| "external-comparison-overlay".to_owned())
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0000_0080))
                .child(
                    div()
                        .id("external-comparison")
                        .debug_selector(|| "external-comparison".to_owned())
                        .w_4_5()
                        .h_4_5()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_2()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(self.theme.header_foreground))
                        .bg(rgb(self.theme.code_background))
                        .text_color(rgb(self.theme.foreground))
                        .child(div().child("差分を確認（この画面では文書を変更しません）"))
                        .when(updated, |element| {
                            element.child(
                                div()
                                    .debug_selector(|| "external-comparison-updated".to_owned())
                                    .child(
                                        "内容が更新されました。最新の内容で再比較してください",
                                    ),
                            )
                        })
                        .child(columns)
                        .child(h_flex().gap_2().child(close).when(updated, |element| {
                            element.child(refresh)
                        })),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comparison_root(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-external-comparison-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// A document with unsaved edits whose file was then changed elsewhere.
    fn open_conflict<'a>(
        cx: &'a mut gpui::TestAppContext,
        path: &Path,
    ) -> (gpui::Entity<EditorView>, &'a mut gpui::VisualTestContext) {
        std::fs::write(path, "before\n").unwrap();
        let loaded = OsFileService.load(path).unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(loaded),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("typed ").unwrap();
            view.after_input(cx);
        });
        std::fs::write(path, "changed elsewhere\n").unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
        });
        cx.run_until_parked();
        (view, cx)
    }

    #[test]
    fn the_changed_span_is_what_lies_between_the_shared_ends() {
        let left = ["a", "b", "c", "d"];
        let right = ["a", "x", "y", "d"];
        assert_eq!(changed_span(&left, &right, true), (1..3, 1..3));
        assert_eq!(changed_span(&left, &right, false), (1..4, 1..4));
        assert_eq!(changed_span(&["a"], &["a", "b"], true), (1..1, 1..2));
        assert_eq!(changed_span(&["a"], &["a"], true), (1..1, 1..1));
    }

    #[test]
    fn a_long_text_is_cut_and_a_crlf_is_not_shown() {
        let text = "one\r\ntwo\n".repeat(MAX_COMPARISON_LINES);
        let (lines, cut) = displayed_lines(&text);
        assert_eq!(lines.len(), MAX_COMPARISON_LINES);
        assert!(cut);
        assert_eq!(lines[0], "one");
        assert_eq!(displayed_lines("a\nb"), (vec!["a", "b"], false));
    }

    #[gpui::test]
    fn opening_and_closing_the_comparison_changes_nothing(cx: &mut gpui::TestAppContext) {
        let root = comparison_root("open");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        let (view, cx) = open_conflict(cx, &path);

        let (text, revision, dirty) = view.read_with(cx, |view, _| {
            (
                view.editor().document().full_text(),
                view.active_session().revision(),
                view.active_session().is_dirty(),
            )
        });
        assert!(dirty);

        view.update(cx, |view, cx| view.open_external_comparison(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let comparison = view.external_comparison.as_ref().expect("opened");
            assert_eq!(comparison.disk, "changed elsewhere\n");
            assert_eq!(comparison.mine, text);
            assert_eq!(
                view.external_comparison_freshness(),
                Some(ComparisonFreshness::Current)
            );
            assert!(!view.external_comparison_is_stale());
        });
        assert!(cx.debug_bounds("external-comparison").is_some());
        assert!(cx.debug_bounds("external-comparison-updated").is_none());

        view.update(cx, |view, cx| view.close_external_comparison(cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("external-comparison").is_none());
        view.read_with(cx, |view, _| {
            assert!(view.external_comparison.is_none());
            assert_eq!(view.editor().document().full_text(), text);
            assert_eq!(view.active_session().revision(), revision);
            assert!(view.active_session().is_dirty());
            assert!(view.active_session().observed_document().is_some());
        });
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "changed elsewhere\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_local_edit_makes_the_comparison_stale_until_it_is_compared_again(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = comparison_root("local-edit");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        let (view, cx) = open_conflict(cx, &path);
        view.update(cx, |view, cx| view.open_external_comparison(cx));

        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("more ").unwrap();
            view.after_input(cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.external_comparison_freshness(),
                Some(ComparisonFreshness::Updated)
            );
            assert!(view.external_comparison_is_stale());
        });
        assert!(cx.debug_bounds("external-comparison-updated").is_some());
        assert!(cx.debug_bounds("external-comparison-refresh").is_some());

        let edited = view.read_with(cx, |view, _| view.editor().document().full_text());
        view.update(cx, |view, cx| view.refresh_external_comparison(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let comparison = view.external_comparison.as_ref().expect("still open");
            assert_eq!(comparison.mine, edited);
            assert_eq!(
                view.external_comparison_freshness(),
                Some(ComparisonFreshness::Current)
            );
            assert_eq!(view.editor().document().full_text(), edited);
        });
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "changed elsewhere\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_new_external_version_makes_the_comparison_stale_and_blocks_resolution(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = comparison_root("external-update");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        let (view, cx) = open_conflict(cx, &path);
        view.update(cx, |view, cx| view.open_external_comparison(cx));

        std::fs::write(&path, "changed a second time\n").unwrap();
        view.update(cx, |view, cx| {
            view.check_open_documents_for_external_changes(cx);
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            assert_eq!(
                view.external_comparison_freshness(),
                Some(ComparisonFreshness::Updated)
            );
            let comparison = view.external_comparison.as_ref().unwrap();
            assert_eq!(comparison.disk, "changed elsewhere\n");
            let revision = view.active_session().revision();
            assert!(view.external_comparison_is_stale());
            // The stale comparison starts no resolution; nothing changes.
            assert_eq!(view.active_session().revision(), revision);
            assert!(view.active_session().is_dirty());

            view.refresh_external_comparison(cx);
            assert_eq!(
                view.external_comparison_freshness(),
                Some(ComparisonFreshness::Current)
            );
            assert_eq!(
                view.external_comparison.as_ref().unwrap().disk,
                "changed a second time\n"
            );
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn the_footer_action_opens_it_and_a_stale_one_refuses_resolution(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = comparison_root("action");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        let (view, cx) = open_conflict(cx, &path);

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.run_resolution(ResolutionAction::Compare, window, cx);
            });
        });
        view.read_with(cx, |view, _| assert!(view.external_comparison.is_some()));

        view.update(cx, |view, cx| {
            view.editor_mut().insert_text("again ").unwrap();
            view.after_input(cx);
        });
        let edited = view.read_with(cx, |view, _| view.editor().document().full_text());
        for action in [
            ResolutionAction::Reload,
            ResolutionAction::Overwrite,
            ResolutionAction::SaveAs,
        ] {
            cx.update(|window, app| {
                view.update(app, |view, cx| view.run_resolution(action, window, cx));
            });
            cx.run_until_parked();
            view.read_with(cx, |view, _| {
                assert_eq!(view.editor().document().full_text(), edited);
                assert!(view.active_session().is_dirty());
                assert!(view.external_comparison_is_stale());
                assert!(view.status.as_deref().is_some_and(|s| s.contains("再比較")));
            });
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "changed elsewhere\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn a_comparison_is_not_shown_for_a_replaced_document(cx: &mut gpui::TestAppContext) {
        let root = comparison_root("replaced");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        let other = root.join("Other.md");
        std::fs::write(&other, "other\n").unwrap();
        let (view, cx) = open_conflict(cx, &path);
        view.update(cx, |view, cx| view.open_external_comparison(cx));

        view.update(cx, |view, cx| {
            let loaded = OsFileService.load(&other).unwrap();
            view.sessions.active_mut().adopt(loaded);
            assert_eq!(
                view.external_comparison_freshness(),
                Some(ComparisonFreshness::Gone)
            );
            assert!(view.external_comparison_is_stale());
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("external-comparison").is_none());
        view.read_with(cx, |view, _| {
            assert!(view.external_comparison.is_none());
            assert_eq!(view.editor().document().full_text(), "other\n");
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn there_is_nothing_to_compare_without_an_observed_disk_version(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = comparison_root("none");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "same\n").unwrap();
        let loaded = OsFileService.load(&path).unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_loaded(loaded),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| view.open_external_comparison(cx));
        view.read_with(cx, |view, _| {
            assert!(view.external_comparison.is_none());
            assert_eq!(view.editor().document().full_text(), "same\n");
            assert!(!view.active_session().is_dirty());
        });
        let _ = std::fs::remove_dir_all(&root);
    }
}
