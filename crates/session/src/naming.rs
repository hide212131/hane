//! H1-derived filenames for work-folder notes (issue #6).
//!
//! Everything here is pure text/path logic with no filesystem access, so the
//! rules — what counts as a title, how it becomes a safe filename, and
//! whether a rename should follow an edited H1 at all — are unit-testable
//! without a work folder. The I/O (probing for a name collision, writing the
//! rename) belongs to the caller.

const MAX_TITLE_CHARS: usize = 100;

/// Extracts the document's title: the first ATX level-1 heading (`# Title`),
/// wherever it appears, ignoring headings inside fenced code blocks so a `#`
/// comment in a code sample is never mistaken for a title. `None` when the
/// document has no such heading, or its text is blank once trimmed.
#[must_use]
pub fn extract_h1_title(text: &str) -> Option<String> {
    let mut in_fence: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(fence) = in_fence {
            if is_fence_close(trimmed, fence) {
                in_fence = None;
            }
            continue;
        }
        if let Some(fence) = fence_open(trimmed) {
            in_fence = Some(fence);
            continue;
        }
        if let Some(title) = atx_h1_text(trimmed)
            && !title.is_empty()
        {
            return Some(title);
        }
    }
    None
}

fn fence_open(line: &str) -> Option<&'static str> {
    if line.starts_with("```") {
        Some("```")
    } else if line.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn is_fence_close(line: &str, fence: &str) -> bool {
    line.starts_with(fence)
}

/// Parses one line as an ATX level-1 heading, per CommonMark: exactly one
/// `#`, then whitespace (or end of line), then content with any trailing run
/// of `#`s (a "closing sequence") stripped when preceded by whitespace.
fn atx_h1_text(line: &str) -> Option<String> {
    let rest = line.strip_prefix('#')?;
    if rest.starts_with('#') {
        return None; // level 2+
    }
    let content = if rest.is_empty() {
        rest
    } else {
        rest.strip_prefix([' ', '\t'])?
    };
    let content = strip_trailing_atx_close(content.trim());
    Some(content.trim().to_owned())
}

/// Strips a CommonMark ATX closing sequence: a trailing run of `#`s preceded
/// by whitespace. A trailing `#` glued directly to the text (`# C#`) is left
/// alone, since CommonMark requires the closing sequence to be set off by a
/// space.
fn strip_trailing_atx_close(content: &str) -> &str {
    let without_hashes = content.trim_end_matches('#');
    if without_hashes.len() == content.len() || without_hashes.is_empty() {
        return content;
    }
    if without_hashes.ends_with([' ', '\t']) {
        without_hashes.trim_end()
    } else {
        content
    }
}

/// Turns a title into a filesystem-safe stem (no extension), or `None` when
/// no safe, non-empty name can be derived. Unicode text (Japanese, emoji,
/// …) is kept as-is: only characters that are actually unsafe as a filename
/// are touched.
#[must_use]
pub fn sanitize_title(title: &str) -> Option<String> {
    let replaced: String = title
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated = truncate_chars(&collapsed, MAX_TITLE_CHARS);
    let trimmed = truncated.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return None;
    }
    Some(trimmed.to_owned())
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_owned()
    } else {
        text.chars().take(max_chars).collect()
    }
}

/// Picks a `.md` filename for `stem` that `taken` (probing full filenames,
/// not stems) reports as free, appending " 2", " 3", … on a collision.
/// Deterministic and never overwrites an existing file.
#[must_use]
pub fn unique_markdown_filename(stem: &str, taken: impl Fn(&str) -> bool) -> String {
    let candidate = format!("{stem}.md");
    if !taken(&candidate) {
        return candidate;
    }
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{stem} {suffix}.md");
        if !taken(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

/// What, if anything, a freshly extracted title should do to a session's
/// filename. Pure decision: no filesystem access, so it can be recomputed
/// cheaply every time the debounce timer fires.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleSyncAction {
    /// No file yet: create one named after the title.
    CreateNamed(String),
    /// Auto-managed, and the title changed: rename to follow it.
    Rename(String),
    /// The current filename no longer matches what auto-management last
    /// derived it from, so the note must stop being auto-managed.
    StopTracking,
    /// Nothing to do.
    None,
}

/// Decides the action for one session.
///
/// - `auto_title` is the title this session's current filename was last
///   derived from, or `None` if the file is not (or no longer) auto-managed.
/// - `current_stem` is the current filename's stem (no extension), or `None`
///   for a session with no file yet.
/// - `extracted_title` is the title just parsed from the live document.
#[must_use]
pub fn decide_title_sync(
    auto_title: Option<&str>,
    current_stem: Option<&str>,
    extracted_title: Option<&str>,
) -> TitleSyncAction {
    // A vanished H1 must not delete or rename away the current file: the
    // note keeps its last real name until a new valid H1 replaces it.
    let Some(sanitized) = extracted_title.and_then(sanitize_title) else {
        return TitleSyncAction::None;
    };
    match (current_stem, auto_title) {
        (None, _) => TitleSyncAction::CreateNamed(sanitized),
        (Some(_), None) => TitleSyncAction::None,
        (Some(stem), Some(tracked)) => {
            let tracked_stem = sanitize_title(tracked).unwrap_or_default();
            if stem != tracked_stem {
                TitleSyncAction::StopTracking
            } else if sanitized == stem {
                TitleSyncAction::None
            } else {
                TitleSyncAction::Rename(sanitized)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_h1_is_extracted_regardless_of_leading_blank_lines() {
        assert_eq!(
            extract_h1_title("\n\n# LangChain4j\n\nbody\n").as_deref(),
            Some("LangChain4j")
        );
    }

    #[test]
    fn only_the_first_of_several_h1s_counts() {
        assert_eq!(extract_h1_title("# One\n\n# Two\n").as_deref(), Some("One"));
    }

    #[test]
    fn a_closing_atx_sequence_is_stripped() {
        assert_eq!(extract_h1_title("# Title #\n").as_deref(), Some("Title"));
        assert_eq!(extract_h1_title("# Title ###\n").as_deref(), Some("Title"));
    }

    #[test]
    fn a_hash_glued_to_a_word_is_kept_not_treated_as_closing() {
        assert_eq!(extract_h1_title("# C#\n").as_deref(), Some("C#"));
    }

    #[test]
    fn level_two_and_deeper_headings_are_not_titles() {
        assert_eq!(extract_h1_title("## Not a title\n"), None);
    }

    #[test]
    fn headings_inside_fenced_code_are_ignored() {
        let text = "```\n# not a title\n```\n\n# Real Title\n";
        assert_eq!(extract_h1_title(text).as_deref(), Some("Real Title"));
    }

    #[test]
    fn an_empty_or_whitespace_only_document_has_no_title() {
        assert_eq!(extract_h1_title(""), None);
        assert_eq!(extract_h1_title("just a paragraph\n"), None);
        assert_eq!(extract_h1_title("#\n"), None);
        assert_eq!(extract_h1_title("#   \n"), None);
    }

    #[test]
    fn unicode_and_emoji_titles_are_kept_as_is() {
        assert_eq!(
            sanitize_title("AI推進室 定例会").as_deref(),
            Some("AI推進室 定例会")
        );
        assert_eq!(sanitize_title("Launch 🚀").as_deref(), Some("Launch 🚀"));
    }

    #[test]
    fn path_separators_and_reserved_characters_are_replaced() {
        assert_eq!(
            sanitize_title("a/b\\c:d*e?f\"g<h>i|j").as_deref(),
            Some("a b c d e f g h i j")
        );
    }

    #[test]
    fn dot_and_dotdot_titles_are_rejected() {
        assert_eq!(sanitize_title("."), None);
        assert_eq!(sanitize_title(".."), None);
        assert_eq!(sanitize_title("   "), None);
        assert_eq!(sanitize_title(""), None);
    }

    #[test]
    fn a_very_long_title_is_truncated() {
        let long = "a".repeat(500);
        let sanitized = sanitize_title(&long).unwrap();
        assert_eq!(sanitized.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn collision_resolution_picks_the_first_free_numbered_name() {
        let taken = |name: &str| matches!(name, "Title.md" | "Title 2.md");
        assert_eq!(unique_markdown_filename("Title", taken), "Title 3.md");
    }

    #[test]
    fn collision_resolution_is_a_no_op_when_the_plain_name_is_free() {
        assert_eq!(unique_markdown_filename("Title", |_| false), "Title.md");
    }

    #[test]
    fn a_brand_new_note_with_a_title_creates_a_named_file() {
        let action = decide_title_sync(None, None, Some("LangChain4j"));
        assert_eq!(action, TitleSyncAction::CreateNamed("LangChain4j".into()));
    }

    #[test]
    fn a_brand_new_note_with_no_title_yet_does_nothing() {
        assert_eq!(decide_title_sync(None, None, None), TitleSyncAction::None);
    }

    #[test]
    fn an_auto_managed_note_follows_its_h1() {
        let action = decide_title_sync(
            Some("LangChain4j"),
            Some("LangChain4j"),
            Some("LangChain4j Agent"),
        );
        assert_eq!(action, TitleSyncAction::Rename("LangChain4j Agent".into()));
    }

    #[test]
    fn an_unchanged_title_needs_no_rename() {
        let action = decide_title_sync(Some("Title"), Some("Title"), Some("Title"));
        assert_eq!(action, TitleSyncAction::None);
    }

    #[test]
    fn a_non_auto_managed_existing_file_is_never_renamed() {
        let action = decide_title_sync(None, Some("2026-08-29-meeting"), Some("AI推進室 定例会"));
        assert_eq!(action, TitleSyncAction::None);
    }

    #[test]
    fn a_drifted_filename_stops_auto_management_instead_of_renaming() {
        // The filename no longer matches what auto-naming last derived it
        // from (an external rename, or a Save As over it), so a further H1
        // edit must not silently start renaming a file the user retitled.
        let action = decide_title_sync(
            Some("LangChain4j"),
            Some("2026-08-29-meeting"),
            Some("LangChain4j Agent"),
        );
        assert_eq!(action, TitleSyncAction::StopTracking);
    }

    #[test]
    fn deleting_the_h1_keeps_the_current_name() {
        let action = decide_title_sync(Some("Title"), Some("Title"), None);
        assert_eq!(action, TitleSyncAction::None);
    }
}
