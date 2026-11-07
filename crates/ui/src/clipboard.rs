//! Clipboard snapshot and paste decisions for rich Paste (Issue #596).
//!
//! The OS read happens once on the UI thread and yields plain text and HTML of
//! the same clipboard generation (macOS `changeCount`, Windows one
//! `ClipboardGuard`). Linux, and every unit test, use the GPUI plain clipboard.
//! Which text is inserted, and what is converted in the background, is decided
//! here by pure functions so the view only has to apply the result.

use hane_markdown::{HtmlImportError, html_to_markdown, html_to_plain_text};

/// Shown when only the HTML was unusable and the plain text of the same
/// clipboard snapshot was pasted instead.
pub(crate) const HTML_FALLBACK_NOTICE: &str =
    "書式を変換できなかったため、プレーンテキストを貼り付けました";
/// Shown when nothing could be pasted from a formatted clipboard.
pub(crate) const HTML_UNSUPPORTED_MESSAGE: &str =
    "書式付きの内容を変換できなかったため、貼り付けませんでした";
pub(crate) const HTML_TOO_LARGE_MESSAGE: &str =
    "書式付きの内容が大きすぎるため、貼り付けませんでした";
pub(crate) const COMPOSING_MESSAGE: &str = "変換を確定してから貼り付けてください";
pub(crate) const CONVERTING_MESSAGE: &str = "貼り付けを変換しています…";
pub(crate) const CLIPBOARD_BUSY_MESSAGE: &str =
    "クリップボードを読み取れませんでした。もう一度お試しください";

/// How the user asked to paste into the body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasteMode {
    /// Ctrl/Cmd+V: external HTML becomes Markdown.
    Rich,
    /// Ctrl/Cmd+Shift+V: the plain text as it is.
    PlainText,
}

/// The representations of one clipboard read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ClipboardSnapshot {
    pub(crate) plain: Option<String>,
    pub(crate) html: Option<String>,
    /// An HTML representation exists but could not be used (invalid CF_HTML
    /// offsets, bad UTF-8, over the size limit). The plain text is still valid.
    pub(crate) html_problem: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "only the macOS and Windows readers (and the test hook) construct these"
)]
pub(crate) enum SnapshotError {
    /// The clipboard could not be opened now.
    Busy,
    /// Another app wrote while the read was running.
    ChangedDuringRead,
}

#[cfg(test)]
thread_local! {
    static TEST_HTML: std::cell::RefCell<(Option<String>, bool)> =
        const { std::cell::RefCell::new((None, false)) };
    static TEST_ERROR: std::cell::Cell<Option<SnapshotError>> =
        const { std::cell::Cell::new(None) };
}

/// Test hook: the HTML (and "HTML was unusable") the next snapshots report, and
/// an error to report instead. The plain text still comes from the GPUI test
/// clipboard.
#[cfg(test)]
pub(crate) fn set_test_snapshot(
    html: Option<&str>,
    html_problem: bool,
    error: Option<SnapshotError>,
) {
    TEST_HTML.with(|cell| *cell.borrow_mut() = (html.map(str::to_owned), html_problem));
    TEST_ERROR.with(|cell| cell.set(error));
}

#[cfg(test)]
pub(crate) fn test_html_snapshot() -> Result<(Option<String>, bool), SnapshotError> {
    if let Some(error) = TEST_ERROR.with(std::cell::Cell::get) {
        return Err(error);
    }
    Ok(TEST_HTML.with(|cell| cell.borrow().clone()))
}

/// The plain-text and HTML decision input derived from the shared native read.
pub(crate) fn text_snapshot(
    plain: Option<String>,
    html: Option<String>,
    html_problem: bool,
) -> ClipboardSnapshot {
    ClipboardSnapshot {
        plain,
        html,
        html_problem,
    }
}

/// HTML that must be converted off the UI thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConvertJob {
    pub(crate) html: String,
    /// The plain text of the same snapshot, used when the HTML cannot be
    /// converted.
    pub(crate) plain: Option<String>,
    pub(crate) mode: PasteMode,
}

/// What a paste does with one snapshot, before any background work.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PasteDecision {
    /// No usable content: the document is left alone.
    Nothing,
    /// Plain text pasted the way typing inserts it (the existing path).
    InsertPlain(String),
    /// Replace the selection with `text` as one independent edit.
    Replace {
        text: String,
        notice: Option<&'static str>,
    },
    /// Leave the document alone and show the reason.
    Refuse(&'static str),
    /// Convert the HTML in the background.
    Convert(ConvertJob),
}

/// The decision table of the design: plain-only text is never treated as HTML;
/// plain-text paste uses the plain text as provided; HTML that is unusable
/// falls back to the plain text of the same snapshot; HTML-only content that
/// cannot be used is refused instead of deleting the selection.
pub(crate) fn decide(mode: PasteMode, snapshot: ClipboardSnapshot) -> PasteDecision {
    let ClipboardSnapshot {
        plain,
        html,
        html_problem,
    } = snapshot;
    let plain = plain.filter(|text| !text.is_empty());
    match mode {
        PasteMode::PlainText => match (plain, html) {
            (Some(plain), _) => PasteDecision::InsertPlain(plain),
            (None, Some(html)) => PasteDecision::Convert(ConvertJob {
                html,
                plain: None,
                mode,
            }),
            (None, None) if html_problem => PasteDecision::Refuse(HTML_UNSUPPORTED_MESSAGE),
            (None, None) => PasteDecision::Nothing,
        },
        PasteMode::Rich => match html {
            Some(html) => PasteDecision::Convert(ConvertJob { html, plain, mode }),
            None if html_problem => match plain {
                Some(text) => PasteDecision::Replace {
                    text,
                    notice: Some(HTML_FALLBACK_NOTICE),
                },
                None => PasteDecision::Refuse(HTML_UNSUPPORTED_MESSAGE),
            },
            None => match plain {
                Some(text) => PasteDecision::InsertPlain(text),
                None => PasteDecision::Nothing,
            },
        },
    }
}

/// The result of a background conversion.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum PasteOutcome {
    Insert {
        text: String,
        notice: Option<&'static str>,
    },
    Refuse(&'static str),
}

fn refusal(error: HtmlImportError) -> &'static str {
    match error {
        HtmlImportError::TooLarge => HTML_TOO_LARGE_MESSAGE,
        HtmlImportError::Empty | HtmlImportError::Unsupported => HTML_UNSUPPORTED_MESSAGE,
    }
}

/// Runs the HTML conversion. Pure and OS-independent; safe off the UI thread.
pub(crate) fn convert(job: ConvertJob) -> PasteOutcome {
    match job.mode {
        PasteMode::PlainText => match html_to_plain_text(&job.html) {
            Ok(text) if !text.is_empty() => PasteOutcome::Insert { text, notice: None },
            Ok(_) => PasteOutcome::Refuse(HTML_UNSUPPORTED_MESSAGE),
            Err(error) => PasteOutcome::Refuse(refusal(error)),
        },
        PasteMode::Rich => match html_to_markdown(&job.html) {
            Ok(markdown) if !markdown.is_empty() => PasteOutcome::Insert {
                text: markdown,
                notice: None,
            },
            result => match job.plain {
                Some(text) => PasteOutcome::Insert {
                    text,
                    notice: Some(HTML_FALLBACK_NOTICE),
                },
                None => PasteOutcome::Refuse(match result {
                    Err(error) => refusal(error),
                    Ok(_) => HTML_UNSUPPORTED_MESSAGE,
                }),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(plain: Option<&str>, html: Option<&str>, problem: bool) -> ClipboardSnapshot {
        ClipboardSnapshot {
            plain: plain.map(str::to_owned),
            html: html.map(str::to_owned),
            html_problem: problem,
        }
    }

    #[test]
    fn plain_only_text_is_never_treated_as_html() {
        let html_looking = "<p>前 <a href=\"https://example.com/\">x</a></p>";
        for mode in [PasteMode::Rich, PasteMode::PlainText] {
            assert_eq!(
                decide(mode, snapshot(Some(html_looking), None, false)),
                PasteDecision::InsertPlain(html_looking.to_owned())
            );
        }
        assert_eq!(decide(PasteMode::Rich, snapshot(None, None, false)), PasteDecision::Nothing);
        assert_eq!(decide(PasteMode::Rich, snapshot(Some(""), None, false)), PasteDecision::Nothing);
    }

    #[test]
    fn rich_paste_converts_html_and_keeps_the_plain_for_fallback() {
        let decision = decide(PasteMode::Rich, snapshot(Some("p"), Some("<b>x</b>"), false));
        assert_eq!(
            decision,
            PasteDecision::Convert(ConvertJob {
                html: "<b>x</b>".to_owned(),
                plain: Some("p".to_owned()),
                mode: PasteMode::Rich,
            })
        );
        // HTML-only is converted too.
        assert!(matches!(
            decide(PasteMode::Rich, snapshot(None, Some("<b>x</b>"), false)),
            PasteDecision::Convert(_)
        ));
    }

    #[test]
    fn plain_text_paste_uses_the_provided_plain_even_with_html() {
        assert_eq!(
            decide(PasteMode::PlainText, snapshot(Some("p"), Some("<b>x</b>"), false)),
            PasteDecision::InsertPlain("p".to_owned())
        );
        // HTML-only: the text is extracted from the HTML.
        assert_eq!(
            decide(PasteMode::PlainText, snapshot(None, Some("<b>x</b>"), false)),
            PasteDecision::Convert(ConvertJob {
                html: "<b>x</b>".to_owned(),
                plain: None,
                mode: PasteMode::PlainText,
            })
        );
    }

    #[test]
    fn unusable_html_falls_back_to_the_same_snapshots_plain_or_refuses() {
        assert_eq!(
            decide(PasteMode::Rich, snapshot(Some("p"), None, true)),
            PasteDecision::Replace {
                text: "p".to_owned(),
                notice: Some(HTML_FALLBACK_NOTICE),
            }
        );
        assert_eq!(
            decide(PasteMode::Rich, snapshot(None, None, true)),
            PasteDecision::Refuse(HTML_UNSUPPORTED_MESSAGE)
        );
        // Plain-text paste does not need the HTML at all.
        assert_eq!(
            decide(PasteMode::PlainText, snapshot(Some("p"), None, true)),
            PasteDecision::InsertPlain("p".to_owned())
        );
    }

    #[test]
    fn conversion_produces_markdown_with_links_and_falls_back_to_plain() {
        let html = r#"<p>前 <a href="https://example.com/a">日本語</a> と <strong>太字</strong>、<a href="https://example.org/">別</a>。後</p>"#;
        assert_eq!(
            convert(ConvertJob {
                html: html.to_owned(),
                plain: Some("前 日本語 と 太字、別。後".to_owned()),
                mode: PasteMode::Rich,
            }),
            PasteOutcome::Insert {
                text: "前 [日本語](https://example.com/a) と **太字**、[別](https://example.org/)。後"
                    .to_owned(),
                notice: None,
            }
        );
        // A table cannot be converted: the same snapshot's plain is pasted with a notice.
        let table = "<table><tr><td>a</td></tr></table>";
        assert_eq!(
            convert(ConvertJob {
                html: table.to_owned(),
                plain: Some("a".to_owned()),
                mode: PasteMode::Rich,
            }),
            PasteOutcome::Insert {
                text: "a".to_owned(),
                notice: Some(HTML_FALLBACK_NOTICE),
            }
        );
        // HTML-only and unconvertible: nothing is pasted.
        assert_eq!(
            convert(ConvertJob {
                html: table.to_owned(),
                plain: None,
                mode: PasteMode::Rich,
            }),
            PasteOutcome::Refuse(HTML_UNSUPPORTED_MESSAGE)
        );
        // Plain-text paste of HTML-only content extracts the visible text.
        assert_eq!(
            convert(ConvertJob {
                html: "<p><b>a</b> *b*</p>".to_owned(),
                plain: None,
                mode: PasteMode::PlainText,
            }),
            PasteOutcome::Insert {
                text: "a *b*".to_owned(),
                notice: None,
            }
        );
    }
}
