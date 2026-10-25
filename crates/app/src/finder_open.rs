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
#[allow(dead_code, reason = "the app opens through `openable_path_from_url`; this stays the Markdown-only contract")]
pub fn markdown_path_from_url(url: &str) -> Option<PathBuf> {
    let path = local_path_from_url(url)?;
    is_markdown(&path).then_some(path)
}

/// Like [`markdown_path_from_url`], but an existing local folder is accepted
/// too (it becomes a Work Folder). Anything else, including a folder that no
/// longer exists, is ignored.
pub fn openable_path_from_url(url: &str) -> Option<PathBuf> {
    let path = local_path_from_url(url)?;
    (is_markdown(&path) || path.is_dir()).then_some(path)
}

/// The one existing absolute folder of a Finder service request. Several
/// items, a file, a relative path or a missing path select nothing.
pub fn single_folder(paths: &[PathBuf]) -> Option<PathBuf> {
    match paths {
        [path] if path.is_absolute() && path.is_dir() => Some(path.clone()),
        _ => None,
    }
}

/// The absolute local path a `file://` URL refers to, whatever it points at.
fn local_path_from_url(url: &str) -> Option<PathBuf> {
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
    path.is_absolute().then_some(path)
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

    fn temp_folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hane-finder-open-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn url_of(path: &Path) -> String {
        use std::fmt::Write;
        use std::os::unix::ffi::OsStrExt;
        let mut url = String::from("file://");
        for byte in path.as_os_str().as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.') {
                url.push(*byte as char);
            } else {
                write!(url, "%{byte:02X}").unwrap();
            }
        }
        url
    }

    #[test]
    fn existing_folder_urls_open_including_spaces_and_japanese_names() {
        let dir = temp_folder("my notes 日本語");
        assert_eq!(openable_path_from_url(&url_of(&dir)), Some(dir.clone()));
        assert_eq!(
            openable_path_from_url(&format!("{}/", url_of(&dir))),
            Some(PathBuf::from(format!("{}/", dir.display())))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_folder_remote_and_malformed_urls_are_not_opened() {
        let dir = temp_folder("gone");
        let url = url_of(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(openable_path_from_url(&url), None);
        let existing = temp_folder("remote");
        let remote = url_of(&existing).replacen("file://", "file://server", 1);
        assert_eq!(openable_path_from_url(&remote), None);
        assert_eq!(openable_path_from_url("https://example.com/tmp"), None);
        assert_eq!(openable_path_from_url("file:///tmp/a%2"), None);
        assert_eq!(openable_path_from_url("file:///tmp/a%00"), None);
        std::fs::remove_dir_all(&existing).unwrap();
    }

    #[test]
    fn openable_urls_still_accept_markdown_and_reject_other_files() {
        assert_eq!(
            openable_path_from_url("file:///nope/note.md"),
            Some(PathBuf::from("/nope/note.md"))
        );
        assert_eq!(openable_path_from_url("file:///nope/note.txt"), None);
    }

    #[test]
    fn a_service_request_selects_exactly_one_existing_folder() {
        let dir = temp_folder("service");
        let other = temp_folder("service-other");
        assert_eq!(single_folder(std::slice::from_ref(&dir)), Some(dir.clone()));
        assert_eq!(single_folder(&[dir.clone(), other.clone()]), None);
        assert_eq!(single_folder(&[]), None);
        assert_eq!(single_folder(&[PathBuf::from("relative")]), None);
        assert_eq!(single_folder(&[dir.join("missing")]), None);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
    }
}
