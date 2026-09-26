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
pub const ICON_SETTINGS: &str = "icons/settings.svg";
pub const ICON_ARROW_LEFT: &str = "icons/arrow-left.svg";
pub const ICON_CHECK: &str = "icons/check.svg";

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
const SETTINGS_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/settings.svg"
));
const ARROW_LEFT_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/arrow-left.svg"
));
const CHECK_SVG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/check.svg"
));

/// Serves the work-folder sidebar icons from memory. Every other asset
/// path falls through to `None`, the same as gpui's default no-op
/// `AssetSource`; `AppAssets` is what composes this with `gpui-component`'s
/// own icon set for paths Hane does not own.
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
            ICON_SETTINGS => Some(Cow::Borrowed(SETTINGS_SVG)),
            ICON_ARROW_LEFT => Some(Cow::Borrowed(ARROW_LEFT_SVG)),
            ICON_CHECK => Some(Cow::Borrowed(CHECK_SVG)),
            _ => None,
        })
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

/// The `AssetSource` registered with the app via `Application::with_assets`.
/// gpui only allows one asset source per app, but Hane's own sidebar icons
/// (`WorkFolderIcons`) and `gpui-component`'s standard icon set (e.g. the
/// `IconName::Copy` glyph used by the file tab `HoverCard`, bundled by the
/// `gpui-kit-assets` crate) are bundled separately, so this tries Hane's own
/// icons first and falls back to `gpui-kit-assets`'s bundled assets for
/// anything Hane does not own.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = WorkFolderIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut entries = WorkFolderIcons.list(path)?;
        entries.extend(gpui_kit_assets::Assets.list(path)?);
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_assets_serves_hane_icons_unchanged() {
        for path in [
            ICON_FILE,
            ICON_FOLDER,
            ICON_FILE_NEW,
            ICON_FOLDER_NEW,
            ICON_CHEVRON_RIGHT,
            ICON_CHEVRON_DOWN,
            ICON_SETTINGS,
            ICON_ARROW_LEFT,
            ICON_CHECK,
        ] {
            assert_eq!(
                AppAssets.load(path).unwrap(),
                WorkFolderIcons.load(path).unwrap(),
                "AppAssets must keep serving Hane's own icon at {path} unchanged"
            );
        }
    }

    // gpui-component's own icon set, resolved through `gpui_kit_assets::Assets`
    // rather than one of Hane's `ICON_*` constants. This is what
    // `IconName::Copy` (the file tab HoverCard's copy button) needs to
    // resolve to an actual SVG instead of gpui's default no-op AssetSource.
    #[test]
    fn app_assets_falls_back_to_component_copy_icon() {
        let path = "icons/copy.svg";
        assert!(
            WorkFolderIcons.load(path).unwrap().is_none(),
            "this path must not collide with one of Hane's own icons"
        );
        let resolved = AppAssets
            .load(path)
            .expect("gpui-kit-assets's Assets must not error for its own bundled copy icon");
        assert!(
            resolved.is_some(),
            "expected gpui-kit-assets's bundled Assets to resolve {path} so IconName::Copy renders"
        );
    }
}
