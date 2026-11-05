//! Issue #673: a bare URL is never rewritten on its way to disk. Saving and
//! reloading keep every source byte, and the metadata regenerated from the
//! reloaded text is the metadata of the text that was saved.

use hane_document::{Revision, SourceOffset, SourceRange};
use hane_editor::{EditorCommand, Selection};
use hane_markdown::{BlockIndex, parse_document};
use hane_session::testing::MemoryFileService;
use hane_session::{FileService, SaveDecision, SaveIntent, SaveOutcome, SessionSet, run_save_job};
use std::path::Path;

fn links(source: &str) -> Vec<(SourceRange, String)> {
    parse_document(Revision(1), SourceRange::new(0, source.len()), source)
        .links
        .iter()
        .map(|link| (link.text_range, link.destination.to_string()))
        .collect()
}

/// The links a formal `BlockIndex` hands to presentation for `source`.
fn projected_links(source: &str) -> Vec<(SourceRange, String)> {
    let index = BlockIndex::build(Revision(1), source);
    let mut found = Vec::new();
    for block in index.blocks() {
        if let Some(projection) = index.list_projection(&block) {
            found.extend(
                projection
                    .links_in(block.source_range)
                    .map(|link| (link.text_range, link.destination.to_string())),
            );
        }
    }
    found
}

fn save_and_assert(
    sessions: &mut SessionSet,
    service: &MemoryFileService,
    path: &Path,
    expected: &str,
) {
    let session = sessions.active_mut();
    let SaveDecision::Write(job) = session.request_save(SaveIntent::Current) else {
        panic!("save must be accepted")
    };
    let result = run_save_job(service, &job.path, &job.document, job.guard);
    assert!(matches!(
        session.finish_save(job.ticket, result),
        SaveOutcome::Saved
    ));
    let saved = service.contents(path).expect("saved contents");
    assert_eq!(saved.as_bytes(), expected.as_bytes());
}

#[test]
fn bare_urls_survive_save_and_reload_byte_for_byte() {
    let urls = [
        "HTTPS://Example.com/A%2f?q=日本語&b=%E3%81%82#Top",
        "https://ok.example/p_(q)",
        "http://example.org/a?b=1&c=2#frag",
    ];
    let source = format!(
        "メモ 🙂 {}。\n\n{} next\n\nsee {}, end\n",
        urls[0], urls[1], urls[2]
    );
    let service = MemoryFileService::new();
    let path = Path::new("/notes/bare-url.md");
    service.write_externally(path, &source);
    let mut sessions = SessionSet::with_loaded(service.load(path).unwrap());

    let original = links(&source);
    assert_eq!(
        original
            .iter()
            .map(|(_, destination)| destination.as_str())
            .collect::<Vec<_>>(),
        urls
    );
    assert_eq!(projected_links(&source), original);

    // An explicit save of an untouched document writes the exact bytes.
    save_and_assert(&mut sessions, &service, path, &source);

    // A UTF-8 edit in front of every URL moves the metadata, never the URL.
    let prefix = "先頭 ";
    let editor = sessions.active_mut().editor_mut();
    editor
        .set_selection(Selection::caret(SourceOffset(0)))
        .unwrap();
    editor.insert_text(prefix).unwrap();
    let edited = format!("{prefix}{source}");
    assert_eq!(editor.document().full_text(), edited);
    sessions.active_mut().note_edit();
    save_and_assert(&mut sessions, &service, path, &edited);

    let reopened = service.load(path).unwrap().document.full_text();
    assert_eq!(reopened, edited);
    let reloaded = links(&reopened);
    assert_eq!(projected_links(&reopened), reloaded);
    assert_eq!(reloaded.len(), original.len());
    for ((before, before_destination), (after, after_destination)) in
        original.iter().zip(&reloaded)
    {
        assert_eq!(before_destination, after_destination);
        assert_eq!(after.start.0, before.start.0 + prefix.len());
        assert_eq!(after.end.0, before.end.0 + prefix.len());
        assert_eq!(&reopened[after.start.0..after.end.0], after_destination);
    }

    // Undo restores the original bytes, which save back unchanged too.
    sessions
        .active_mut()
        .editor_mut()
        .dispatch(EditorCommand::Undo)
        .unwrap();
    sessions.active_mut().note_edit();
    save_and_assert(&mut sessions, &service, path, &source);
    let restored = service.load(path).unwrap().document.full_text();
    assert_eq!(links(&restored), original);
}
