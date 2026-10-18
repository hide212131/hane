//! Relative path validation shared by ZIP entries, inventories, plans and
//! journals. Rules are deliberately stricter than any one OS: names are
//! printable ASCII only, which also rules out Unicode-normalization collisions
//! without a normalization dependency.

use crate::error::{ErrorCode, Result, UpdateError};
use crate::limits::{MAX_COMPONENT_BYTES, MAX_PATH_BYTES};

fn reject(path: &str, why: &str) -> UpdateError {
    UpdateError::new(
        ErrorCode::ArchiveRejected,
        format!("unsafe path {path:?}: {why}"),
    )
}

const RESERVED_DEVICE_NAMES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

fn is_reserved_device(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if RESERVED_DEVICE_NAMES.contains(&stem.as_str()) {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = stem.strip_prefix(prefix)
            && rest.len() == 1
            && matches!(rest.as_bytes()[0], b'1'..=b'9')
        {
            return true;
        }
    }
    false
}

pub fn validate_component(path: &str, component: &str) -> Result<()> {
    if component.is_empty() {
        return Err(reject(path, "empty component"));
    }
    if component == "." || component == ".." {
        return Err(reject(path, "dot component"));
    }
    if component.len() > MAX_COMPONENT_BYTES {
        return Err(reject(path, "component too long"));
    }
    for ch in component.chars() {
        if !ch.is_ascii() {
            return Err(reject(path, "non-ASCII name"));
        }
        if ch.is_ascii_control() {
            return Err(reject(path, "control character"));
        }
        if matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/') {
            return Err(reject(path, "forbidden character"));
        }
    }
    if component.ends_with('.') || component.ends_with(' ') {
        return Err(reject(path, "trailing dot or space"));
    }
    if is_reserved_device(component) {
        return Err(reject(path, "reserved device name"));
    }
    if component.eq_ignore_ascii_case("__MACOSX") || component.eq_ignore_ascii_case(".DS_Store") {
        return Err(reject(path, "archiver metadata"));
    }
    Ok(())
}

/// Validates a forward-slash relative path without trailing separator.
pub fn validate_rel_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(reject(path, "empty path"));
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(reject(path, "path too long"));
    }
    if path.starts_with('/') || path.ends_with('/') {
        return Err(reject(path, "absolute path or trailing separator"));
    }
    for component in path.split('/') {
        validate_component(path, component)?;
    }
    Ok(())
}

/// Key under which two names are considered the same entry on a
/// case-insensitive filesystem.
pub fn collision_key(path: &str) -> String {
    path.to_ascii_lowercase()
}

/// All strict ancestors of `path` (`a/b/c` -> `a`, `a/b`).
pub fn ancestors(path: &str) -> Vec<&str> {
    path.match_indices('/').map(|(i, _)| &path[..i]).collect()
}
