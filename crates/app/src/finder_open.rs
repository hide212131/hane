//! Converts the file URLs macOS hands to the app (Finder's "Open With",
//! double-click on an associated document) into paths for the editor.
//!
//! Only local `file://` URLs for Markdown documents are accepted; anything
//! else (other schemes, remote hosts, other extensions) is ignored rather
//! than opened.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some(decoded)
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// Returns the absolute local Markdown path a `file://` URL refers to, or
/// `None` for anything Hane should not open.
pub fn markdown_path_from_url(url: &str) -> Option<PathBuf> {
    let scheme = url.get(..7)?;
    if !scheme.eq_ignore_ascii_case("file://") {
        return None;
    }
    let rest = &url[7..];
    let path_start = rest.find('/')?;
    let authority = &rest[..path_start];
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return None;
    }
    let decoded = percent_decode(&rest[path_start..])?;
    if decoded.contains(&0) {
        return None;
    }
    let path = PathBuf::from(OsString::from_vec(decoded));
    (path.is_absolute() && is_markdown(&path)).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_file_url_becomes_path() {
        assert_eq!(
            markdown_path_from_url("file:///Users/me/note.md"),
            Some(PathBuf::from("/Users/me/note.md"))
        );
    }

    #[test]
    fn localhost_authority_and_uppercase_extension_are_accepted() {
        assert_eq!(
            markdown_path_from_url("file://localhost/tmp/NOTE.MD"),
            Some(PathBuf::from("/tmp/NOTE.MD"))
        );
    }

    #[test]
    fn percent_escapes_are_decoded_including_utf8() {
        assert_eq!(
            markdown_path_from_url("file:///tmp/my%20notes/%E6%97%A5%E8%A8%98.md"),
            Some(PathBuf::from("/tmp/my notes/日記.md"))
        );
    }

    #[test]
    fn non_file_schemes_and_remote_hosts_are_rejected() {
        assert_eq!(markdown_path_from_url("https://example.com/a.md"), None);
        assert_eq!(markdown_path_from_url("file://server/share/a.md"), None);
        assert_eq!(markdown_path_from_url("hane:///tmp/a.md"), None);
        assert_eq!(markdown_path_from_url("file"), None);
    }

    #[test]
    fn non_markdown_and_malformed_urls_are_rejected() {
        assert_eq!(markdown_path_from_url("file:///tmp/a.txt"), None);
        assert_eq!(markdown_path_from_url("file:///tmp/dir/"), None);
        assert_eq!(markdown_path_from_url("file:///tmp/a%2.md"), None);
        assert_eq!(markdown_path_from_url("file:///tmp/a%00.md"), None);
        assert_eq!(markdown_path_from_url("file://"), None);
    }
}
