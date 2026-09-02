//! SVG icons for the work folder sidebar, bundled into the binary so a
//! `svg()` element can reference them by a stable virtual path regardless of
//! the process's working directory or whether the app runs from an
//! installed location. `gpui::svg()` resolves paths through the app's
//! registered `AssetSource`; `WorkFolderIcons` is that source, registered in
//! `main.rs` via `Application::with_assets`.

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// A Markdown file row in the sidebar tree.
pub const ICON_FILE: &str = "icons/work-folder/file.svg";
/// The work folder root row, or a folder row in the sidebar tree.
pub const ICON_FOLDER: &str = "icons/work-folder/folder.svg";
/// The "new note" toolbar action.
pub const ICON_FILE_NEW: &str = "icons/work-folder/file-new.svg";
/// The "new folder" toolbar action.
pub const ICON_FOLDER_NEW: &str = "icons/work-folder/folder-new.svg";
/// Collapsed folder disclosure chevron.
pub const ICON_CHEVRON_RIGHT: &str = "icons/work-folder/chevron-right.svg";
/// Expanded folder disclosure chevron.
pub const ICON_CHEVRON_DOWN: &str = "icons/work-folder/chevron-down.svg";

const FILE_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/work-folder/file.svg"
));
const FOLDER_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/work-folder/folder.svg"
));
const FILE_NEW_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/work-folder/file-new.svg"
));
const FOLDER_NEW_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/work-folder/folder-new.svg"
));
const CHEVRON_RIGHT_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/work-folder/chevron-right.svg"
));
const CHEVRON_DOWN_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/work-folder/chevron-down.svg"
));

/// Serves the work-folder sidebar icons from memory. Every other asset
/// path (there are none yet, but a future one) resolves to `None`, the same
/// as gpui's default no-op `AssetSource`.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkFolderIcons;

impl AssetSource for WorkFolderIcons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            ICON_FILE => Some(Cow::Borrowed(FILE_SVG)),
            ICON_FOLDER => Some(Cow::Borrowed(FOLDER_SVG)),
            ICON_FILE_NEW => Some(Cow::Borrowed(FILE_NEW_SVG)),
            ICON_FOLDER_NEW => Some(Cow::Borrowed(FOLDER_NEW_SVG)),
            ICON_CHEVRON_RIGHT => Some(Cow::Borrowed(CHEVRON_RIGHT_SVG)),
            ICON_CHEVRON_DOWN => Some(Cow::Borrowed(CHEVRON_DOWN_SVG)),
            _ => None,
        })
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
