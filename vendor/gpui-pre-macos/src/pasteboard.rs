use core::slice;
use std::ffi::{CStr, c_void};
use std::path::PathBuf;

use cocoa::{
    appkit::{
        NSFilenamesPboardType, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString,
        NSPasteboardTypeTIFF,
    },
    base::{id, nil},
    foundation::{NSArray, NSData, NSFastEnumeration, NSString},
};
use objc::{msg_send, rc::StrongPtr, runtime::Object, sel, sel_impl};
use smallvec::SmallVec;

use crate::ns_string;
use gpui::{
    ClipboardEntry, ClipboardItem, ClipboardString, ExternalPaths, Image, ImageFormat, hash,
};

/// Raster formats first (PNG carries alpha), SVG last: it is not a raster image.
const HANE_IMAGE_FORMAT_PRIORITY: [ImageFormat; 9] = [
    ImageFormat::Png,
    ImageFormat::Tiff,
    ImageFormat::Jpeg,
    ImageFormat::Gif,
    ImageFormat::Bmp,
    ImageFormat::Webp,
    ImageFormat::Ico,
    ImageFormat::Pnm,
    ImageFormat::Svg,
];

/// Hane: the largest single image representation taken from the pasteboard
/// (64 MiB, the same bound the editor applies to an input image).
const HANE_MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// Largest HTML representation read from the pasteboard (UTF-8 bytes).
pub const MAX_RICH_HTML_BYTES: usize = 8 * 1024 * 1024;

/// Hane: why a rich clipboard read could not produce its content. Kept
/// identical in shape to the Windows crate's type so the editor handles both
/// the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RichClipboardError {
    /// The clipboard could not be opened (never produced on macOS).
    Busy,
    /// The pasteboard generation kept changing during the bounded retries.
    ChangedDuringRead,
    /// A representation is present but its data is missing or empty.
    InvalidFormat,
    /// A representation exceeds the size bound.
    TooLarge,
}

impl std::fmt::Display for RichClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Busy => "the clipboard is busy",
            Self::ChangedDuringRead => "the clipboard changed while it was read",
            Self::InvalidFormat => "the clipboard data is not valid",
            Self::TooLarge => "the clipboard data is too large",
        })
    }
}

impl std::error::Error for RichClipboardError {}

/// Hane: everything of one pasteboard generation, in one value.
#[derive(Clone, Debug, PartialEq)]
pub struct RichClipboardSnapshot {
    /// The plain text and the file paths of the generation.
    pub item: ClipboardItem,
    /// The UTF-8 HTML fragment from the same pasteboard generation.
    pub html: Option<String>,
    /// Why the HTML was unusable, while other representations remain available.
    pub html_problem: Option<RichClipboardError>,
    /// One entry per independent pasteboard item that carries an image. The
    /// formats of the same item collapse to the preferred one; separate items
    /// stay separate. An unreadable image is an `Err`, never silently dropped.
    pub images: Vec<Result<Image, RichClipboardError>>,
}

/// Reads the general pasteboard (Hane Issue #596). Call on the main thread.
pub fn read_rich_clipboard() -> Result<Option<RichClipboardSnapshot>, RichClipboardError> {
    Pasteboard::general().read_rich()
}

/// Hane: an image data length the editor can accept.
fn check_image_length(length: usize) -> Result<(), RichClipboardError> {
    if length == 0 {
        Err(RichClipboardError::InvalidFormat)
    } else if length > HANE_MAX_IMAGE_BYTES {
        Err(RichClipboardError::TooLarge)
    } else {
        Ok(())
    }
}

/// Hane: the bytes of an `NSData`, bounded before they are copied.
unsafe fn hane_image_bytes(data: id) -> Result<Vec<u8>, RichClipboardError> {
    unsafe {
        if data == nil {
            return Err(RichClipboardError::InvalidFormat);
        }
        let length = data.length() as usize;
        check_image_length(length)?;
        let bytes = data.bytes();
        if bytes.is_null() {
            return Err(RichClipboardError::InvalidFormat);
        }
        Ok(slice::from_raw_parts(bytes as *const u8, length).to_vec())
    }
}

/// Hane: the preferred image of one pasteboard owner (the pasteboard itself or
/// one `NSPasteboardItem`; both answer `types` and `dataForType:`). `None`
/// when it has no image type at all.
unsafe fn hane_best_image(owner: id) -> Option<Result<Image, RichClipboardError>> {
    unsafe {
        let types: id = msg_send![owner, types];
        if types == nil {
            return None;
        }
        let mut failure = None;
        for format in HANE_IMAGE_FORMAT_PRIORITY {
            let ut_type: UTType = format.into();
            let present: bool = msg_send![types, containsObject: ut_type.inner()];
            if !present {
                continue;
            }
            let data: id = msg_send![owner, dataForType: ut_type.inner_mut()];
            match hane_image_bytes(data) {
                Ok(bytes) => {
                    // A readable SVG never hides a raster failure.
                    if format == ImageFormat::Svg && failure.is_some() {
                        break;
                    }
                    let id = hash(&bytes);
                    return Some(Ok(Image { format, bytes, id }));
                }
                // An unreadable SVG is not an image failure.
                Err(error) if format != ImageFormat::Svg => {
                    failure = failure.or(Some(error));
                }
                Err(_) => {}
            }
        }
        failure.map(Err)
    }
}

pub struct Pasteboard {
    inner: StrongPtr,
    text_hash_type: StrongPtr,
    metadata_type: StrongPtr,
}

impl Pasteboard {
    pub fn general() -> Self {
        unsafe { Self::new(NSPasteboard::generalPasteboard(nil)) }
    }

    pub fn find() -> Self {
        unsafe { Self::new(NSPasteboard::pasteboardWithName(nil, NSPasteboardNameFind)) }
    }

    #[cfg(test)]
    pub fn unique() -> Self {
        unsafe { Self::new(NSPasteboard::pasteboardWithUniqueName(nil)) }
    }

    unsafe fn new(inner: id) -> Self {
        // These constructors return autoreleased objects, but a Pasteboard can
        // outlive the autorelease pool in which it was created.
        Self {
            inner: unsafe { StrongPtr::retain(inner) },
            text_hash_type: unsafe { StrongPtr::retain(ns_string("zed-text-hash")) },
            metadata_type: unsafe { StrongPtr::retain(ns_string("zed-metadata")) },
        }
    }

    /// Hane: reads one pasteboard generation. The item is only returned when
    /// `changeCount` is the same before and after the read, so text and image
    /// representations always come from the same generation. A generation that
    /// keeps changing is reported as "nothing" after a bounded number of tries.
    pub fn read(&self) -> Option<ClipboardItem> {
        const ATTEMPTS: usize = 3;
        for _ in 0..ATTEMPTS {
            let before: isize = unsafe { msg_send![*self.inner, changeCount] };
            let item = self.read_generation();
            let after: isize = unsafe { msg_send![*self.inner, changeCount] };
            if before == after {
                return item;
            }
        }
        None
    }

    /// Hane: reads text, file paths and every independent image of the
    /// pasteboard, keeping the failures apart from a really empty pasteboard.
    /// The result is only returned when `changeCount` is the same before and
    /// after the read; a generation that keeps changing is
    /// `ChangedDuringRead` after a bounded number of tries, never "empty".
    pub fn read_rich(&self) -> Result<Option<RichClipboardSnapshot>, RichClipboardError> {
        const ATTEMPTS: usize = 3;
        for _ in 0..ATTEMPTS {
            let before: isize = unsafe { msg_send![*self.inner, changeCount] };
            let snapshot = self.read_rich_generation();
            let after: isize = unsafe { msg_send![*self.inner, changeCount] };
            if before == after {
                return Ok(snapshot);
            }
        }
        Err(RichClipboardError::ChangedDuringRead)
    }

    fn read_rich_generation(&self) -> Option<RichClipboardSnapshot> {
        unsafe {
            let mut entries = Vec::new();
            if let Some(paths) = self.read_paths() {
                entries.push(paths);
            }
            if let Some(string_entry) = self.read_string_from_pasteboard() {
                entries.push(string_entry);
            }
            let images = self.read_independent_images();
            let (html, html_problem) = match self.read_html() {
                Ok(html) => (html, None),
                Err(problem) => (None, Some(problem)),
            };
            if entries.is_empty() && images.is_empty() && html.is_none() && html_problem.is_none() {
                return None;
            }
            Some(RichClipboardSnapshot {
                item: ClipboardItem { entries },
                html,
                html_problem,
                images,
            })
        }
    }

    /// One preferred image per `NSPasteboardItem`. A pasteboard that reports no
    /// items is treated as one item of its own.
    unsafe fn read_independent_images(&self) -> Vec<Result<Image, RichClipboardError>> {
        unsafe {
            let items: id = msg_send![*self.inner, pasteboardItems];
            let count = if items == nil {
                0
            } else {
                NSArray::count(items)
            };
            let mut images = Vec::new();
            if count == 0 {
                if let Some(image) = hane_best_image(*self.inner) {
                    images.push(image);
                }
                return images;
            }
            for index in 0..count {
                let item = NSArray::objectAtIndex(items, index);
                if let Some(image) = hane_best_image(item) {
                    images.push(image);
                }
            }
            images
        }
    }

    unsafe fn read_html(&self) -> Result<Option<String>, RichClipboardError> {
        unsafe {
            let pasteboard_types: id = self.inner.types();
            let html_type: id = ns_string("public.html");
            if !msg_send![pasteboard_types, containsObject: html_type] {
                return Ok(None);
            }
            let Some(bytes) = self.limited_data_for_type(html_type, MAX_RICH_HTML_BYTES)? else {
                return Ok(None);
            };
            let html = String::from_utf8(bytes).map_err(|_| RichClipboardError::InvalidFormat)?;
            Ok(Some(
                html.strip_prefix('\u{feff}')
                    .map(str::to_owned)
                    .unwrap_or(html),
            ))
        }
    }

    /// The native length is checked before the data is copied into Rust memory.
    unsafe fn limited_data_for_type(
        &self,
        kind: id,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, RichClipboardError> {
        unsafe {
            let data = self.inner.dataForType(kind);
            if data == nil {
                return Ok(None);
            }
            let length = data.length() as usize;
            if length > max_bytes {
                return Err(RichClipboardError::TooLarge);
            }
            if length == 0 || data.bytes().is_null() {
                return Ok(Some(Vec::new()));
            }
            Ok(Some(
                slice::from_raw_parts(data.bytes() as *const u8, length).to_vec(),
            ))
        }
    }

    unsafe fn read_paths(&self) -> Option<ClipboardEntry> {
        unsafe {
            let filenames = NSPasteboard::propertyListForType(*self.inner, NSFilenamesPboardType);
            if filenames != nil && NSArray::count(filenames) > 0 {
                let mut paths = SmallVec::new();
                for file in filenames.iter() {
                    let f = NSString::UTF8String(file);
                    let path = CStr::from_ptr(f).to_string_lossy().into_owned();
                    paths.push(PathBuf::from(path));
                }
                if !paths.is_empty() {
                    return Some(ClipboardEntry::ExternalPaths(ExternalPaths(paths)));
                }
            }
            None
        }
    }
    fn read_generation(&self) -> Option<ClipboardItem> {
        unsafe {
            // Check for file paths first
            if let Some(paths) = self.read_paths() {
                let mut entries = vec![paths];

                // Also include the string representation so text editors can
                // paste the path as text.
                if let Some(string_item) = self.read_string_from_pasteboard() {
                    entries.push(string_item);
                }

                return Some(ClipboardItem { entries });
            }

            // Next, the plain string and, from the same generation, one image
            // (Hane: a text + image clipboard keeps both so the editor can
            // prefer the image without reading the pasteboard a second time).
            let mut entries = Vec::new();
            if let Some(string_entry) = self.read_string_from_pasteboard() {
                entries.push(string_entry);
            }
            for format in HANE_IMAGE_FORMAT_PRIORITY {
                if let Some(item) = self.read_image(format) {
                    entries.extend(item.entries);
                    break;
                }
            }
            if !entries.is_empty() {
                return Some(ClipboardItem { entries });
            }
        }

        None
    }

    fn read_image(&self, format: ImageFormat) -> Option<ClipboardItem> {
        let ut_type: UTType = format.into();

        unsafe {
            let types: id = self.inner.types();
            if msg_send![types, containsObject: ut_type.inner()] {
                self.data_for_type(ut_type.inner_mut()).map(|bytes| {
                    let bytes = bytes.to_vec();
                    let id = hash(&bytes);

                    ClipboardItem {
                        entries: vec![ClipboardEntry::Image(Image { format, bytes, id })],
                    }
                })
            } else {
                None
            }
        }
    }

    unsafe fn read_string_from_pasteboard(&self) -> Option<ClipboardEntry> {
        unsafe {
            let pasteboard_types: id = self.inner.types();
            let string_type: id = ns_string("public.utf8-plain-text");

            if !msg_send![pasteboard_types, containsObject: string_type] {
                return None;
            }

            let text_bytes = self.data_for_type(string_type)?;

            let text = String::from_utf8_lossy(&text_bytes).to_string();
            let metadata = self
                .data_for_type(*self.text_hash_type)
                .and_then(|hash_bytes| {
                    let hash_bytes = hash_bytes.as_slice().try_into().ok()?;
                    let hash = u64::from_be_bytes(hash_bytes);
                    let metadata = self.data_for_type(*self.metadata_type)?;

                    if hash == ClipboardString::text_hash(&text) {
                        String::from_utf8(metadata).ok()
                    } else {
                        None
                    }
                });

            Some(ClipboardEntry::String(ClipboardString { text, metadata }))
        }
    }

    unsafe fn data_for_type(&self, kind: id) -> Option<Vec<u8>> {
        unsafe {
            let data = self.inner.dataForType(kind);
            if data == nil {
                None
            } else if data.bytes().is_null() {
                Some(Vec::new())
            } else {
                Some(
                    slice::from_raw_parts(data.bytes() as *mut u8, data.length() as usize).to_vec(),
                )
            }
        }
    }

    pub fn write(&self, item: ClipboardItem) {
        unsafe {
            match item.entries.as_slice() {
                [] => {
                    // Writing an empty list of entries just clears the clipboard.
                    self.inner.clearContents();
                }
                [ClipboardEntry::String(string)] => {
                    self.write_plaintext(string);
                }
                [ClipboardEntry::Image(image)] => {
                    self.write_image(image);
                }
                [ClipboardEntry::ExternalPaths(_)] => {}
                _ => {
                    // Agus NB: We're currently only writing string entries to the clipboard when we have more than one.
                    //
                    // This was the existing behavior before I refactored the outer clipboard code:
                    // https://github.com/zed-industries/zed/blob/65f7412a0265552b06ce122655369d6cc7381dd6/crates/gpui/src/platform/mac/platform.rs#L1060-L1110
                    //
                    // Note how `any_images` is always `false`. We should fix that, but that's orthogonal to the refactor.

                    let mut combined = ClipboardString {
                        text: String::new(),
                        metadata: None,
                    };

                    for entry in item.entries {
                        match entry {
                            ClipboardEntry::String(text) => {
                                combined.text.push_str(&text.text());
                                if combined.metadata.is_none() {
                                    combined.metadata = text.metadata;
                                }
                            }
                            _ => {}
                        }
                    }

                    self.write_plaintext(&combined);
                }
            }
        }
    }

    fn write_plaintext(&self, string: &ClipboardString) {
        unsafe {
            self.inner.clearContents();

            let text_bytes = NSData::dataWithBytes_length_(
                nil,
                string.text.as_ptr() as *const c_void,
                string.text.len() as u64,
            );
            self.inner
                .setData_forType(text_bytes, NSPasteboardTypeString);

            if let Some(metadata) = string.metadata.as_ref() {
                let hash_bytes = ClipboardString::text_hash(&string.text).to_be_bytes();
                let hash_bytes = NSData::dataWithBytes_length_(
                    nil,
                    hash_bytes.as_ptr() as *const c_void,
                    hash_bytes.len() as u64,
                );
                self.inner.setData_forType(hash_bytes, *self.text_hash_type);

                let metadata_bytes = NSData::dataWithBytes_length_(
                    nil,
                    metadata.as_ptr() as *const c_void,
                    metadata.len() as u64,
                );
                self.inner
                    .setData_forType(metadata_bytes, *self.metadata_type);
            }
        }
    }

    unsafe fn write_image(&self, image: &Image) {
        unsafe {
            self.inner.clearContents();

            let bytes = NSData::dataWithBytes_length_(
                nil,
                image.bytes.as_ptr() as *const c_void,
                image.bytes.len() as u64,
            );

            self.inner
                .setData_forType(bytes, Into::<UTType>::into(image.format).inner_mut());
        }
    }
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    /// [Apple's documentation](https://developer.apple.com/documentation/appkit/nspasteboardnamefind?language=objc)
    pub static NSPasteboardNameFind: id;
}

impl From<ImageFormat> for UTType {
    fn from(value: ImageFormat) -> Self {
        match value {
            ImageFormat::Png => Self::png(),
            ImageFormat::Jpeg => Self::jpeg(),
            ImageFormat::Tiff => Self::tiff(),
            ImageFormat::Webp => Self::webp(),
            ImageFormat::Gif => Self::gif(),
            ImageFormat::Bmp => Self::bmp(),
            ImageFormat::Svg => Self::svg(),
            ImageFormat::Ico => Self::ico(),
            ImageFormat::Pnm => Self::pnm(),
        }
    }
}

// See https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/
pub struct UTType(id);

impl UTType {
    pub fn png() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/png
        Self(unsafe { NSPasteboardTypePNG }) // This is a rare case where there's a built-in NSPasteboardType
    }

    pub fn jpeg() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/jpeg
        Self(unsafe { ns_string("public.jpeg") })
    }

    pub fn gif() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/gif
        Self(unsafe { ns_string("com.compuserve.gif") })
    }

    pub fn webp() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/webp
        Self(unsafe { ns_string("org.webmproject.webp") })
    }

    pub fn bmp() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/bmp
        Self(unsafe { ns_string("com.microsoft.bmp") })
    }

    pub fn svg() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/svg
        Self(unsafe { ns_string("public.svg-image") })
    }

    pub fn ico() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/ico
        Self(unsafe { ns_string("com.microsoft.ico") })
    }

    pub fn tiff() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/tiff
        Self(unsafe { NSPasteboardTypeTIFF }) // This is a rare case where there's a built-in NSPasteboardType
    }

    pub fn pnm() -> Self {
        //https://en.wikipedia.org/w/index.php?title=Netpbm&oldid=1336679433 under Uniform Type Identifier
        Self(unsafe { ns_string("public.pbm") })
    }

    fn inner(&self) -> *const Object {
        self.0
    }

    pub fn inner_mut(&self) -> *mut Object {
        self.0 as *mut _
    }
}

#[cfg(test)]
mod tests {
    use cocoa::{
        appkit::{NSFilenamesPboardType, NSPasteboard, NSPasteboardTypeString},
        base::{id, nil},
        foundation::{NSArray, NSData},
    };
    use std::ffi::c_void;

    use gpui::{ClipboardEntry, ClipboardItem, ClipboardString, ImageFormat};
    use objc::{class, rc::autoreleasepool};

    use super::*;

    unsafe fn simulate_external_file_copy(pasteboard: &Pasteboard, paths: &[&str]) {
        unsafe {
            let ns_paths: Vec<id> = paths.iter().map(|p| ns_string(p)).collect();
            let ns_array = NSArray::arrayWithObjects(nil, &ns_paths);

            let mut types = vec![NSFilenamesPboardType];
            types.push(NSPasteboardTypeString);

            let types_array = NSArray::arrayWithObjects(nil, &types);
            pasteboard.inner.declareTypes_owner(types_array, nil);

            pasteboard
                .inner
                .setPropertyList_forType(ns_array, NSFilenamesPboardType);

            let joined = paths.join("\n");
            let bytes = NSData::dataWithBytes_length_(
                nil,
                joined.as_ptr() as *const c_void,
                joined.len() as u64,
            );
            pasteboard
                .inner
                .setData_forType(bytes, NSPasteboardTypeString);
        }
    }

    #[test]
    fn test_string() {
        let pasteboard = Pasteboard::unique();
        assert_eq!(pasteboard.read(), None);

        let item = ClipboardItem::new_string("1".to_string());
        pasteboard.write(item.clone());
        assert_eq!(pasteboard.read(), Some(item));

        let item = ClipboardItem {
            entries: vec![ClipboardEntry::String(
                ClipboardString::new("2".to_string()).with_json_metadata(vec![3, 4]),
            )],
        };
        pasteboard.write(item.clone());
        assert_eq!(pasteboard.read(), Some(item));

        let text_from_other_app = "text from other app";
        unsafe {
            let bytes = NSData::dataWithBytes_length_(
                nil,
                text_from_other_app.as_ptr() as *const c_void,
                text_from_other_app.len() as u64,
            );
            pasteboard
                .inner
                .setData_forType(bytes, NSPasteboardTypeString);
        }
        assert_eq!(
            pasteboard.read(),
            Some(ClipboardItem::new_string(text_from_other_app.to_string()))
        );
    }

    #[test]
    fn test_custom_types_survive_creation_autorelease_pool() {
        let pasteboard = autoreleasepool(|| unsafe { Pasteboard::new(nil) });

        unsafe {
            let text_hash_type = CStr::from_ptr(NSString::UTF8String(*pasteboard.text_hash_type));
            let metadata_type = CStr::from_ptr(NSString::UTF8String(*pasteboard.metadata_type));
            assert_eq!(text_hash_type.to_bytes(), b"zed-text-hash");
            assert_eq!(metadata_type.to_bytes(), b"zed-metadata");
        }
    }

    #[test]
    fn test_read_external_path() {
        let pasteboard = Pasteboard::unique();

        unsafe {
            simulate_external_file_copy(&pasteboard, &["/test.txt"]);
        }

        let item = pasteboard.read().expect("should read clipboard item");

        // Test both ExternalPaths and String entries exist
        assert_eq!(item.entries.len(), 2);

        // Test first entry is ExternalPaths
        match &item.entries[0] {
            ClipboardEntry::ExternalPaths(ep) => {
                assert_eq!(ep.paths(), &[PathBuf::from("/test.txt")]);
            }
            other => panic!("expected ExternalPaths, got {:?}", other),
        }

        // Test second entry is String
        match &item.entries[1] {
            ClipboardEntry::String(s) => {
                assert_eq!(s.text(), "/test.txt");
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    #[test]
    fn test_read_external_paths_with_spaces() {
        let pasteboard = Pasteboard::unique();
        let paths = ["/some file with spaces.txt"];

        unsafe {
            simulate_external_file_copy(&pasteboard, &paths);
        }

        let item = pasteboard.read().expect("should read clipboard item");

        match &item.entries[0] {
            ClipboardEntry::ExternalPaths(ep) => {
                assert_eq!(ep.paths(), &[PathBuf::from("/some file with spaces.txt")]);
            }
            other => panic!("expected ExternalPaths, got {:?}", other),
        }
    }

    #[test]
    fn test_read_multiple_external_paths() {
        let pasteboard = Pasteboard::unique();
        let paths = ["/file.txt", "/image.png"];

        unsafe {
            simulate_external_file_copy(&pasteboard, &paths);
        }

        let item = pasteboard.read().expect("should read clipboard item");
        assert_eq!(item.entries.len(), 2);

        // Test both ExternalPaths and String entries exist
        match &item.entries[0] {
            ClipboardEntry::ExternalPaths(ep) => {
                assert_eq!(
                    ep.paths(),
                    &[PathBuf::from("/file.txt"), PathBuf::from("/image.png"),]
                );
            }
            other => panic!("expected ExternalPaths, got {:?}", other),
        }

        match &item.entries[1] {
            ClipboardEntry::String(s) => {
                assert_eq!(s.text(), "/file.txt\n/image.png");
                assert_eq!(s.metadata, None);
            }
            other => panic!("expected String, got {:?}", other),
        }
    }

    unsafe fn hane_clipboard_publish(pasteboard: &Pasteboard, entries: &[(id, &[u8])]) {
        unsafe {
            let types: Vec<id> = entries.iter().map(|(kind, _)| *kind).collect();
            let types_array = NSArray::arrayWithObjects(nil, &types);
            pasteboard.inner.declareTypes_owner(types_array, nil);
            for (kind, bytes) in entries {
                let data = NSData::dataWithBytes_length_(
                    nil,
                    bytes.as_ptr() as *const c_void,
                    bytes.len() as u64,
                );
                pasteboard.inner.setData_forType(data, *kind);
            }
        }
    }

    #[test]
    fn hane_clipboard_reads_plain_and_html_from_one_generation() {
        let pasteboard = Pasteboard::unique();
        unsafe {
            let html_type = ns_string("public.html");
            hane_clipboard_publish(
                &pasteboard,
                &[
                    (NSPasteboardTypeString, "リンク".as_bytes()),
                    (html_type, "<p><a href=\"https://example.com/\">リンク</a></p>".as_bytes()),
                ],
            );
        }
        let snapshot = pasteboard.read_rich().unwrap().expect("snapshot");
        assert_eq!(snapshot.item.text().as_deref(), Some("リンク"));
        assert_eq!(
            snapshot.html.as_deref(),
            Some("<p><a href=\"https://example.com/\">リンク</a></p>")
        );
        assert_eq!(snapshot.html_problem, None);
    }

    #[test]
    fn hane_clipboard_reads_html_only() {
        let pasteboard = Pasteboard::unique();
        assert!(pasteboard.read_rich().unwrap().is_none());
        unsafe {
            let html_type = ns_string("public.html");
            hane_clipboard_publish(&pasteboard, &[(html_type, "<b>x</b>".as_bytes())]);
        }
        let snapshot = pasteboard.read_rich().unwrap().expect("snapshot");
        assert!(snapshot.item.entries.is_empty());
        assert_eq!(snapshot.html.as_deref(), Some("<b>x</b>"));
    }

    #[test]
    fn hane_clipboard_keeps_plain_when_html_is_invalid_or_too_large() {
        let pasteboard = Pasteboard::unique();
        unsafe {
            let html_type = ns_string("public.html");
            hane_clipboard_publish(
                &pasteboard,
                &[
                    (NSPasteboardTypeString, b"plain".as_slice()),
                    (html_type, [0xff_u8, 0xfe, 0x00, 0x80].as_slice()),
                ],
            );
        }
        let snapshot = pasteboard.read_rich().unwrap().expect("snapshot");
        assert_eq!(snapshot.item.text().as_deref(), Some("plain"));
        assert_eq!(snapshot.html, None);
        assert_eq!(snapshot.html_problem, Some(RichClipboardError::InvalidFormat));

        let pasteboard = Pasteboard::unique();
        let big = vec![b'a'; MAX_RICH_HTML_BYTES + 1];
        unsafe {
            let html_type = ns_string("public.html");
            hane_clipboard_publish(
                &pasteboard,
                &[
                    (NSPasteboardTypeString, b"plain".as_slice()),
                    (html_type, big.as_slice()),
                ],
            );
        }
        let snapshot = pasteboard.read_rich().unwrap().expect("snapshot");
        assert_eq!(snapshot.item.text().as_deref(), Some("plain"));
        assert_eq!(snapshot.html, None);
        assert_eq!(snapshot.html_problem, Some(RichClipboardError::TooLarge));
    }

    #[test]
    fn hane_clipboard_plain_only_has_no_html() {
        let pasteboard = Pasteboard::unique();
        pasteboard.write(ClipboardItem::new_string("<p>not html</p>".to_string()));
        let snapshot = pasteboard.read_rich().unwrap().expect("snapshot");
        assert_eq!(snapshot.item.text().as_deref(), Some("<p>not html</p>"));
        assert_eq!(snapshot.html, None);
        assert_eq!(snapshot.html_problem, None);
    }

    #[test]
    fn test_read_image() {
        let pasteboard = Pasteboard::unique();

        // Smallest valid PNG: 1x1 transparent pixel
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x62, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xE5, 0x27, 0xDE, 0xFC, 0x00, 0x00,
            0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        unsafe {
            let ns_png_type = NSPasteboardTypePNG;
            let types_array = NSArray::arrayWithObjects(nil, &[ns_png_type]);
            pasteboard.inner.declareTypes_owner(types_array, nil);

            let data = NSData::dataWithBytes_length_(
                nil,
                png_bytes.as_ptr() as *const c_void,
                png_bytes.len() as u64,
            );
            pasteboard.inner.setData_forType(data, ns_png_type);
        }

        let item = pasteboard.read().expect("should read PNG image");

        // Test Image entry exists
        assert_eq!(item.entries.len(), 1);
        match &item.entries[0] {
            ClipboardEntry::Image(img) => {
                assert_eq!(img.format, ImageFormat::Png);
                assert_eq!(img.bytes, png_bytes);
            }
            other => panic!("expected Image, got {:?}", other),
        }
    }

    // Hane: a clipboard holding both a string and an image returns both from
    // one generation, so the editor never has to read the pasteboard twice.
    #[test]
    fn hane_read_keeps_text_and_image_of_one_generation() {
        let pasteboard = Pasteboard::unique();
        let png_bytes: &[u8] = &[0x89, 0x50, 0x4E, 0x47];

        unsafe {
            let ns_png_type = NSPasteboardTypePNG;
            let types_array = NSArray::arrayWithObjects(nil, &[NSPasteboardTypeString, ns_png_type]);
            pasteboard.inner.declareTypes_owner(types_array, nil);
            let text = NSData::dataWithBytes_length_(nil, b"cell".as_ptr() as *const c_void, 4);
            pasteboard.inner.setData_forType(text, NSPasteboardTypeString);
            let data = NSData::dataWithBytes_length_(
                nil,
                png_bytes.as_ptr() as *const c_void,
                png_bytes.len() as u64,
            );
            pasteboard.inner.setData_forType(data, ns_png_type);
        }

        let item = pasteboard.read().expect("should read text and image");
        assert_eq!(item.entries.len(), 2);
        match (&item.entries[0], &item.entries[1]) {
            (ClipboardEntry::String(text), ClipboardEntry::Image(image)) => {
                assert_eq!(text.text(), "cell");
                assert_eq!(image.format, ImageFormat::Png);
                assert_eq!(image.bytes, png_bytes);
            }
            other => panic!("expected String then Image, got {:?}", other),
        }
    }

    unsafe fn hane_data(bytes: &[u8]) -> id {
        unsafe {
            NSData::dataWithBytes_length_(nil, bytes.as_ptr() as *const c_void, bytes.len() as u64)
        }
    }

    #[test]
    fn hane_clipboard_empty_pasteboard_is_ok_none() {
        let pasteboard = Pasteboard::unique();
        assert_eq!(pasteboard.read_rich(), Ok(None));
    }

    #[test]
    fn hane_clipboard_image_length_bounds() {
        assert_eq!(check_image_length(0), Err(RichClipboardError::InvalidFormat));
        assert_eq!(check_image_length(1), Ok(()));
        assert_eq!(check_image_length(HANE_MAX_IMAGE_BYTES), Ok(()));
        assert_eq!(
            check_image_length(HANE_MAX_IMAGE_BYTES + 1),
            Err(RichClipboardError::TooLarge)
        );
    }

    // paths, plain text and a real image of one generation are all returned;
    // the image is not dropped because paths exist.
    #[test]
    fn hane_clipboard_rich_keeps_paths_plain_and_image() {
        let pasteboard = Pasteboard::unique();
        let png_bytes: &[u8] = &[0x89, 0x50, 0x4E, 0x47];
        unsafe {
            let ns_paths = NSArray::arrayWithObjects(nil, &[ns_string("/a.png")]);
            let types = NSArray::arrayWithObjects(
                nil,
                &[NSFilenamesPboardType, NSPasteboardTypeString, NSPasteboardTypePNG],
            );
            pasteboard.inner.declareTypes_owner(types, nil);
            pasteboard
                .inner
                .setPropertyList_forType(ns_paths, NSFilenamesPboardType);
            pasteboard
                .inner
                .setData_forType(hane_data(b"/a.png"), NSPasteboardTypeString);
            pasteboard
                .inner
                .setData_forType(hane_data(png_bytes), NSPasteboardTypePNG);
        }

        let snapshot = pasteboard
            .read_rich()
            .expect("readable")
            .expect("not empty");
        assert!(matches!(
            snapshot.item.entries[0],
            ClipboardEntry::ExternalPaths(_)
        ));
        assert_eq!(snapshot.item.text().as_deref(), Some("/a.png"));
        assert_eq!(snapshot.images.len(), 1);
        let image = snapshot.images[0].as_ref().expect("a readable image");
        assert_eq!(image.format, ImageFormat::Png);
        assert_eq!(image.bytes, png_bytes);
    }

    // Two formats of the same item are one image (PNG wins).
    #[test]
    fn hane_clipboard_formats_of_one_item_are_one_image() {
        let pasteboard = Pasteboard::unique();
        unsafe {
            let types = NSArray::arrayWithObjects(nil, &[NSPasteboardTypeTIFF, NSPasteboardTypePNG]);
            pasteboard.inner.declareTypes_owner(types, nil);
            pasteboard
                .inner
                .setData_forType(hane_data(b"tiff-bytes"), NSPasteboardTypeTIFF);
            pasteboard
                .inner
                .setData_forType(hane_data(b"png-bytes"), NSPasteboardTypePNG);
        }
        let snapshot = pasteboard.read_rich().unwrap().unwrap();
        assert_eq!(snapshot.images.len(), 1);
        let image = snapshot.images[0].as_ref().unwrap();
        assert_eq!(image.format, ImageFormat::Png);
        assert_eq!(image.bytes, b"png-bytes");
    }

    // Independent pasteboard items stay independent: none is dropped or merged.
    #[test]
    fn hane_clipboard_independent_items_are_reported_separately() {
        let pasteboard = Pasteboard::unique();
        unsafe {
            let first: id = msg_send![class!(NSPasteboardItem), new];
            let second: id = msg_send![class!(NSPasteboardItem), new];
            let first_data = hane_data(b"first");
            let second_data = hane_data(b"second");
            let _: bool = msg_send![first, setData: first_data forType: NSPasteboardTypePNG];
            let _: bool = msg_send![second, setData: second_data forType: NSPasteboardTypePNG];
            let items = NSArray::arrayWithObjects(nil, &[first, second]);
            pasteboard.inner.clearContents();
            let _: bool = msg_send![*pasteboard.inner, writeObjects: items];
        }
        let snapshot = pasteboard.read_rich().unwrap().unwrap();
        let bytes: Vec<&[u8]> = snapshot
            .images
            .iter()
            .map(|image| image.as_ref().unwrap().bytes.as_slice())
            .collect();
        assert_eq!(bytes, vec![b"first".as_slice(), b"second".as_slice()]);
    }
}
