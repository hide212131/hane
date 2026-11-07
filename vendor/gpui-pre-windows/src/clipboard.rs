use std::sync::LazyLock;

use anyhow::Result;
use collections::FxHashMap;
use itertools::Itertools;
use windows::Win32::{
    Foundation::{HANDLE, HGLOBAL},
    System::{
        DataExchange::{
            CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats,
            GetClipboardData, GetClipboardFormatNameW, OpenClipboard, RegisterClipboardFormatW,
            SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT},
    },
    UI::Shell::{DragQueryFileW, HDROP},
};
use windows::core::{Owned, PCWSTR};

use gpui::{
    ClipboardEntry, ClipboardItem, ClipboardString, ExternalPaths, Image, ImageFormat, hash,
};

const DRAGDROP_GET_FILES_COUNT: u32 = 0xFFFFFFFF;

static CLIPBOARD_HASH_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("GPUI internal text hash")));
static CLIPBOARD_METADATA_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("GPUI internal metadata")));
static CLIPBOARD_HTML_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("HTML Format")));
static CLIPBOARD_SVG_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("image/svg+xml")));
static CLIPBOARD_GIF_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("GIF")));
static CLIPBOARD_PNG_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("PNG")));
static CLIPBOARD_JPG_FORMAT: LazyLock<u32> =
    LazyLock::new(|| register_clipboard_format(windows::core::w!("JFIF")));

static IMAGE_FORMATS_MAP: LazyLock<FxHashMap<u32, ImageFormat>> = LazyLock::new(|| {
    let mut map = FxHashMap::default();
    map.insert(*CLIPBOARD_PNG_FORMAT, ImageFormat::Png);
    map.insert(*CLIPBOARD_GIF_FORMAT, ImageFormat::Gif);
    map.insert(*CLIPBOARD_JPG_FORMAT, ImageFormat::Jpeg);
    map.insert(*CLIPBOARD_SVG_FORMAT, ImageFormat::Svg);
    map
});

fn register_clipboard_format(format: PCWSTR) -> u32 {
    let ret = unsafe { RegisterClipboardFormatW(format) };
    if ret == 0 {
        panic!(
            "Error when registering clipboard format: {}",
            std::io::Error::last_os_error()
        );
    }
    log::debug!(
        "Registered clipboard format {} as {}",
        unsafe { format.display() },
        ret
    );
    ret
}

fn get_clipboard_data(format: u32) -> Option<LockedGlobal> {
    let global = HGLOBAL(unsafe { GetClipboardData(format).ok() }?.0);
    LockedGlobal::lock(global)
}

/// Hane: the largest single image representation taken from the clipboard
/// (64 MiB, the same bound the editor applies to an input image).
const HANE_MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// Hane: why a rich clipboard read could not produce its content. Kept
/// identical in shape to the macOS crate's type so the editor handles both
/// the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RichClipboardError {
    /// The clipboard could not be opened after the bounded retries.
    Busy,
    /// The clipboard changed while it was read (never produced on Windows: one
    /// `ClipboardGuard` holds the clipboard for the whole read).
    ChangedDuringRead,
    /// A representation is present but its data is missing or invalid.
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

pub(crate) fn write_to_clipboard(item: ClipboardItem) {
    let Ok(_clip) = ClipboardGuard::open() else {
        return;
    };

    let result: Result<()> = (|| {
        unsafe { EmptyClipboard()? };
        for entry in item.entries() {
            match entry {
                ClipboardEntry::String(string) => write_string(string)?,
                ClipboardEntry::Image(image) => write_image(image)?,
                ClipboardEntry::ExternalPaths(_) => {}
            }
        }
        Ok(())
    })();

    if let Err(e) = result {
        log::error!("Failed to write to clipboard: {e}");
    }
}

pub(crate) fn read_from_clipboard() -> Option<ClipboardItem> {
    let _clip = ClipboardGuard::open().ok()?;
    let ClipboardRead {
        mut entries, image, ..
    } = read_formats(false);
    if let Some(Ok(image)) = image {
        entries.push(ClipboardEntry::Image(image));
    }

    if entries.is_empty() {
        log_unsupported_clipboard_formats();
        return None;
    }
    Some(ClipboardItem { entries })
}

/// Hane: what one pass over the clipboard formats found.
struct ClipboardRead {
    entries: Vec<ClipboardEntry>,
    image: Option<Result<Image, RichClipboardError>>,
    html: Option<String>,
    html_problem: Option<RichClipboardError>,
}

/// Hane: reads every representation of the one clipboard generation. The
/// caller holds the `ClipboardGuard`, so nothing can change in between. One
/// image is kept: PNG (the only representation that carries alpha losslessly)
/// beats the other formats, and CF_DIB (no alpha) is the last resort. A
/// representation that cannot be read only becomes an error when no other
/// representation of the image could be.
fn read_formats(include_html: bool) -> ClipboardRead {
    let mut entries = Vec::new();
    let mut have_text = false;
    let mut have_files = false;
    let mut best_image: Option<(u8, Image)> = None;
    let mut image_failure: Option<RichClipboardError> = None;
    let mut html = None;
    let mut html_problem = None;
    let mut have_html = false;
    // An SVG is not a raster image: it never decides the image failure, and a
    // readable one never hides a raster failure.
    let mut svg_image: Option<Image> = None;

    let count = unsafe { CountClipboardFormats() };
    let mut format = 0;
    for _ in 0..count {
        format = unsafe { EnumClipboardFormats(format) };

        if !have_text && format == CF_UNICODETEXT.0 as u32 {
            if let Some(entry) = read_string() {
                entries.push(entry);
                have_text = true;
            }
        } else if include_html && !have_html && format == *CLIPBOARD_HTML_FORMAT {
            have_html = true;
            match read_html_fragment() {
                Ok(fragment) => html = Some(fragment),
                Err(problem) => html_problem = Some(problem),
            }
        } else if is_svg_format(format) {
            if svg_image.is_none() {
                svg_image = read_image_checked(format).ok();
            }
        } else if is_image_format(format) {
            let priority = image_format_priority(format);
            if best_image
                .as_ref()
                .is_none_or(|(best, _)| priority > *best)
            {
                match read_image_checked(format) {
                    Ok(image) => best_image = Some((priority, image)),
                    Err(error) => image_failure = image_failure.or(Some(error)),
                }
            }
        } else if !have_files && format == CF_HDROP.0 as u32 {
            if let Some(entry) = read_files() {
                entries.push(entry);
                have_files = true;
            }
        }
    }
    let image = match best_image {
        Some((_, image)) => Some(Ok(image)),
        None => match image_failure {
            Some(error) => Some(Err(error)),
            None => svg_image.map(Ok),
        },
    };
    ClipboardRead {
        entries,
        image,
        html,
        html_problem,
    }
}

/// Largest HTML fragment read from the clipboard (UTF-8 bytes).
pub const MAX_RICH_HTML_BYTES: usize = 8 * 1024 * 1024;

/// Extra allocation allowed around the fragment for the CF_HTML header and
/// context before the whole clipboard block is refused as too large.
const CF_HTML_OVERHEAD_BYTES: usize = 64 * 1024;

/// Representations read under one clipboard lock (Hane Issue #596). HTML is the
/// UTF-8 fragment without the CF_HTML header. When it is unusable it is `None`
/// and `html_problem` says why while `item` keeps the plain text, images and
/// files of the same read.
#[derive(Clone, Debug)]
pub struct RichClipboardSnapshot {
    pub item: ClipboardItem,
    pub html: Option<String>,
    pub html_problem: Option<RichClipboardError>,
    /// At most one entry on Windows: the clipboard is a single data object, so
    /// the OS cannot tell independent images apart.
    pub images: Vec<Result<Image, RichClipboardError>>,
}

/// Reads plain text, HTML, images and files while holding the clipboard once.
pub fn read_rich_clipboard() -> Result<Option<RichClipboardSnapshot>, RichClipboardError> {
    let _clip = ClipboardGuard::open()?;
    let read = read_formats(true);
    let has_image = read.image.is_some();
    if read.entries.is_empty()
        && !has_image
        && read.html.is_none()
        && read.html_problem.is_none()
    {
        return Ok(None);
    }
    Ok(Some(RichClipboardSnapshot {
        item: ClipboardItem {
            entries: read.entries,
        },
        html: read.html,
        html_problem: read.html_problem,
        images: read.image.into_iter().collect(),
    }))
}

fn read_html_fragment() -> Result<String, RichClipboardError> {
    let locked = get_clipboard_data(*CLIPBOARD_HTML_FORMAT).ok_or(RichClipboardError::InvalidFormat)?;
    // The native length is checked before anything is copied.
    if locked.size > MAX_RICH_HTML_BYTES + CF_HTML_OVERHEAD_BYTES {
        return Err(RichClipboardError::TooLarge);
    }
    let fragment = parse_cf_html(locked.as_bytes())?;
    if fragment.len() > MAX_RICH_HTML_BYTES {
        return Err(RichClipboardError::TooLarge);
    }
    Ok(fragment.to_owned())
}

fn parse_cf_offset(value: &[u8]) -> Result<i64, RichClipboardError> {
    std::str::from_utf8(value)
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok())
        .ok_or(RichClipboardError::InvalidFormat)
}

/// Extracts the fragment from a CF_HTML block. The header is ASCII and its
/// offsets are UTF-8 *byte* offsets into the whole block; `0 <= StartFragment
/// <= EndFragment <= length` and a valid UTF-8 range are required. A broken
/// offset is never rescued by searching for the fragment markers.
fn parse_cf_html(bytes: &[u8]) -> Result<&str, RichClipboardError> {
    const HEADER_SCAN_LIMIT: usize = 4096;

    let length = bytes.len() as i64;
    let mut start_html = None;
    let mut end_html = None;
    let mut start_fragment = None;
    let mut end_fragment = None;
    let mut position = 0;
    while position < bytes.len() && position < HEADER_SCAN_LIMIT {
        let line_end = bytes[position..]
            .iter()
            .position(|&byte| byte == b'\r' || byte == b'\n')
            .map_or(bytes.len(), |at| position + at);
        let line = &bytes[position..line_end];
        let Some(colon) = line.iter().position(|&byte| byte == b':') else {
            break;
        };
        let key = &line[..colon];
        if key.is_empty() || !key.iter().all(u8::is_ascii_alphabetic) {
            break;
        }
        match key {
            b"StartHTML" => start_html = Some(parse_cf_offset(&line[colon + 1..])?),
            b"EndHTML" => end_html = Some(parse_cf_offset(&line[colon + 1..])?),
            b"StartFragment" => start_fragment = Some(parse_cf_offset(&line[colon + 1..])?),
            b"EndFragment" => end_fragment = Some(parse_cf_offset(&line[colon + 1..])?),
            _ => {}
        }
        position = line_end;
        if bytes.get(position) == Some(&b'\r') && bytes.get(position + 1) == Some(&b'\n') {
            position += 2;
        } else if position < bytes.len() {
            position += 1;
        }
        // The header ends with the fragment offsets; never read fragment
        // content as header lines.
        if start_fragment.is_some() && end_fragment.is_some() {
            break;
        }
    }
    if start_html.is_some_and(|offset| offset < -1) || end_html.is_some_and(|offset| offset < -1) {
        return Err(RichClipboardError::InvalidFormat);
    }

    let (Some(start), Some(end)) = (start_fragment, end_fragment) else {
        return Err(RichClipboardError::InvalidFormat);
    };
    if start < 0 || end < start || end > length || (start as usize) < position {
        return Err(RichClipboardError::InvalidFormat);
    }
    // The context, when present, must contain the fragment.
    if let Some(context_start) = start_html
        && context_start >= 0
        && context_start > start
    {
        return Err(RichClipboardError::InvalidFormat);
    }
    if let Some(context_end) = end_html
        && context_end >= 0
        && (context_end < end || context_end > length)
    {
        return Err(RichClipboardError::InvalidFormat);
    }
    std::str::from_utf8(&bytes[start as usize..end as usize])
        .map_err(|_| RichClipboardError::InvalidFormat)
}

pub(crate) fn with_file_names<F>(hdrop: HDROP, mut f: F)
where
    F: FnMut(String),
{
    let file_count = unsafe { DragQueryFileW(hdrop, DRAGDROP_GET_FILES_COUNT, None) };
    for file_index in 0..file_count {
        let filename_length = unsafe { DragQueryFileW(hdrop, file_index, None) } as usize;
        let mut buffer = vec![0u16; filename_length + 1];
        let ret = unsafe { DragQueryFileW(hdrop, file_index, Some(buffer.as_mut_slice())) };
        if ret == 0 {
            log::error!("unable to read file name of dragged file");
            continue;
        }
        match String::from_utf16(&buffer[0..filename_length]) {
            Ok(file_name) => f(file_name),
            Err(e) => log::error!("dragged file name is not UTF-16: {}", e),
        }
    }
}

fn set_clipboard_bytes<T>(data: &[T], format: u32) -> Result<()> {
    unsafe {
        let global = Owned::new(GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(data))?);
        let ptr = GlobalLock(*global);
        anyhow::ensure!(!ptr.is_null(), "GlobalLock returned null");
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as _, data.len());
        GlobalUnlock(*global).ok();
        SetClipboardData(format, Some(HANDLE(global.0)))?;
        // SetClipboardData succeeded — the system now owns the memory.
        std::mem::forget(global);
    }
    Ok(())
}

fn get_clipboard_string(format: u32) -> Option<String> {
    let locked = get_clipboard_data(format)?;
    let bytes = locked.as_bytes();
    let words_len = bytes.len() / std::mem::size_of::<u16>();
    if words_len == 0 {
        return Some(String::new());
    }
    let slice = unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u16, words_len) };
    let actual_len = slice.iter().position(|&c| c == 0).unwrap_or(words_len);
    Some(String::from_utf16_lossy(&slice[..actual_len]))
}

fn is_image_format(format: u32) -> bool {
    IMAGE_FORMATS_MAP.contains_key(&format) || format == CF_DIB.0 as u32
}

/// Hane: the SVG representation, whose read failure is not an image failure.
fn is_svg_format(format: u32) -> bool {
    IMAGE_FORMATS_MAP.get(&format) == Some(&ImageFormat::Svg)
}

/// Higher wins. SVG is never preferred over a raster representation.
fn image_format_priority(format: u32) -> u8 {
    if format == *CLIPBOARD_PNG_FORMAT {
        4
    } else if format == *CLIPBOARD_JPG_FORMAT || format == *CLIPBOARD_GIF_FORMAT {
        3
    } else if format == CF_DIB.0 as u32 {
        2
    } else {
        1
    }
}

fn write_string(item: &ClipboardString) -> Result<()> {
    let wide: Vec<u16> = item.text.encode_utf16().chain(Some(0)).collect_vec();
    set_clipboard_bytes(&wide, CF_UNICODETEXT.0 as u32)?;

    if let Some(metadata) = item.metadata.as_ref() {
        let hash_bytes = ClipboardString::text_hash(&item.text).to_ne_bytes();
        set_clipboard_bytes(&hash_bytes, *CLIPBOARD_HASH_FORMAT)?;

        let wide: Vec<u16> = metadata.encode_utf16().chain(Some(0)).collect_vec();
        set_clipboard_bytes(&wide, *CLIPBOARD_METADATA_FORMAT)?;
    }
    Ok(())
}

fn write_image(item: &Image) -> Result<()> {
    let native_format = match item.format {
        ImageFormat::Svg => Some(*CLIPBOARD_SVG_FORMAT),
        ImageFormat::Gif => Some(*CLIPBOARD_GIF_FORMAT),
        ImageFormat::Png => Some(*CLIPBOARD_PNG_FORMAT),
        ImageFormat::Jpeg => Some(*CLIPBOARD_JPG_FORMAT),
        _ => None,
    };
    if let Some(format) = native_format {
        set_clipboard_bytes(item.bytes(), format)?;
    }

    // Also provide a PNG copy for broad compatibility.
    // SVG can't be rasterized by the image crate, so skip it.
    if item.format != ImageFormat::Svg && native_format != Some(*CLIPBOARD_PNG_FORMAT) {
        if let Some(png_bytes) = convert_to_png(item.bytes(), item.format) {
            set_clipboard_bytes(&png_bytes, *CLIPBOARD_PNG_FORMAT)?;
        }
    }
    Ok(())
}

fn convert_to_png(bytes: &[u8], format: ImageFormat) -> Option<Vec<u8>> {
    let img_format = gpui_to_image_format(format)?;
    let image = image::load_from_memory_with_format(bytes, img_format)
        .map_err(|e| log::warn!("Failed to decode image for PNG conversion: {e}"))
        .ok()?;
    let mut buf = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .map_err(|e| log::warn!("Failed to encode PNG: {e}"))
        .ok()?;
    Some(buf)
}

fn read_string() -> Option<ClipboardEntry> {
    let text = get_clipboard_string(CF_UNICODETEXT.0 as u32)?;
    let metadata = read_clipboard_metadata(&text);
    Some(ClipboardEntry::String(ClipboardString { text, metadata }))
}

fn read_clipboard_metadata(text: &str) -> Option<String> {
    let locked = get_clipboard_data(*CLIPBOARD_HASH_FORMAT)?;
    let hash_bytes: [u8; 8] = locked.as_bytes().get(..8)?.try_into().ok()?;
    let hash = u64::from_ne_bytes(hash_bytes);
    if hash != ClipboardString::text_hash(text) {
        return None;
    }
    get_clipboard_string(*CLIPBOARD_METADATA_FORMAT)
}

/// Hane: one image representation, bounded before it is copied. A format that
/// is announced but whose data cannot be locked, is empty or is not a valid DIB
/// is `InvalidFormat`, and an oversized one is `TooLarge`.
fn read_image_checked(format: u32) -> Result<Image, RichClipboardError> {
    let locked = get_clipboard_data(format).ok_or(RichClipboardError::InvalidFormat)?;
    check_image_length(locked.as_bytes().len())?;
    let (bytes, image_format) = if format == CF_DIB.0 as u32 {
        (
            convert_dib_to_bmp(locked.as_bytes()).ok_or(RichClipboardError::InvalidFormat)?,
            ImageFormat::Bmp,
        )
    } else {
        let image_format = *IMAGE_FORMATS_MAP
            .get(&format)
            .ok_or(RichClipboardError::InvalidFormat)?;
        (locked.as_bytes().to_vec(), image_format)
    };
    let id = hash(&bytes);
    Ok(Image {
        format: image_format,
        bytes,
        id,
    })
}

fn read_files() -> Option<ClipboardEntry> {
    let locked = get_clipboard_data(CF_HDROP.0 as u32)?;
    let hdrop = HDROP(locked.ptr as *mut _);
    let mut filenames = Vec::new();
    with_file_names(hdrop, |name| filenames.push(std::path::PathBuf::from(name)));
    Some(ClipboardEntry::ExternalPaths(ExternalPaths(
        filenames.into(),
    )))
}

/// DIB is BMP without the 14-byte BITMAPFILEHEADER. Prepend one.
fn convert_dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }

    let header_size = u32::from_le_bytes(dib[0..4].try_into().ok()?);
    if header_size < 40 || header_size as usize > dib.len() {
        return None;
    }
    let bit_count = u16::from_le_bytes(dib[14..16].try_into().ok()?);
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);

    let color_table_size = if bit_count <= 8 {
        let colors_used = u32::from_le_bytes(dib[32..36].try_into().ok()?);
        (if colors_used == 0 {
            1u32 << bit_count
        } else {
            colors_used
        })
        .checked_mul(4)?
    } else if compression == 3 && header_size == 40 {
        // BI_BITFIELDS: BITMAPINFOHEADER の RGB masks だけがヘッダー外に続く。
        // V4 / V5 の masks はヘッダー内に含まれるため加算しない。
        12
    } else {
        0
    };

    let pixel_offset = 14u32
        .checked_add(header_size)?
        .checked_add(color_table_size)?;
    // 画素開始位置が入力の外なら、途中切れの DIB として拒否する。
    if pixel_offset as usize - 14 > dib.len() {
        return None;
    }
    let file_size = 14u32.checked_add(u32::try_from(dib.len()).ok()?)?;

    let mut bmp = Vec::with_capacity(file_size as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 4]); // reserved
    bmp.extend_from_slice(&pixel_offset.to_le_bytes());
    bmp.extend_from_slice(dib);
    Some(bmp)
}

fn log_unsupported_clipboard_formats() {
    let count = unsafe { CountClipboardFormats() };
    let mut format = 0;
    for _ in 0..count {
        format = unsafe { EnumClipboardFormats(format) };
        let mut buffer = [0u16; 64];
        unsafe { GetClipboardFormatNameW(format, &mut buffer) };
        let format_name = String::from_utf16_lossy(&buffer);
        log::warn!(
            "Try to paste with unsupported clipboard format: {}, {}.",
            format,
            format_name
        );
    }
}

fn gpui_to_image_format(value: ImageFormat) -> Option<image::ImageFormat> {
    match value {
        ImageFormat::Png => Some(image::ImageFormat::Png),
        ImageFormat::Jpeg => Some(image::ImageFormat::Jpeg),
        ImageFormat::Webp => Some(image::ImageFormat::WebP),
        ImageFormat::Gif => Some(image::ImageFormat::Gif),
        ImageFormat::Bmp => Some(image::ImageFormat::Bmp),
        ImageFormat::Tiff => Some(image::ImageFormat::Tiff),
        other => {
            log::warn!("No image crate equivalent for format: {other:?}");
            None
        }
    }
}

struct ClipboardGuard;

impl ClipboardGuard {
    /// Another process may hold the clipboard for a moment while it publishes
    /// its data; retry a bounded number of times before giving up. The final
    /// failure is `Busy`, which callers must not treat as an empty clipboard.
    fn open() -> Result<Self, RichClipboardError> {
        const ATTEMPTS: u32 = 5;
        for attempt in 1..=ATTEMPTS {
            match unsafe { OpenClipboard(None) } {
                Ok(()) => return Ok(Self),
                Err(e) if attempt == ATTEMPTS => {
                    log::error!("Failed to open clipboard: {e}");
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        Err(RichClipboardError::Busy)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        if let Err(e) = unsafe { CloseClipboard() } {
            log::error!("Failed to close clipboard: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CF_HTML block like the ones browsers publish: byte offsets, CRLF
    /// header, and a trailing NUL inside the allocation.
    fn cf_html(fragment: &str) -> Vec<u8> {
        let header = |sh: usize, eh: usize, sf: usize, ef: usize| {
            format!(
                "Version:0.9\r\nStartHTML:{sh:010}\r\nEndHTML:{eh:010}\r\nStartFragment:{sf:010}\r\nEndFragment:{ef:010}\r\n"
            )
        };
        let prefix = "<html><body>\r\n<!--StartFragment-->";
        let suffix = "<!--EndFragment-->\r\n</body></html>";
        let start_html = header(0, 0, 0, 0).len();
        let start_fragment = start_html + prefix.len();
        let end_fragment = start_fragment + fragment.len();
        let end_html = end_fragment + suffix.len();
        let mut out = header(start_html, end_html, start_fragment, end_fragment).into_bytes();
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(fragment.as_bytes());
        out.extend_from_slice(suffix.as_bytes());
        out.push(0);
        out
    }

    #[test]
    fn hane_clipboard_cf_html_offsets_are_utf8_bytes() {
        let fragment = "<p>日本語 <a href=\"https://example.com/\">リンク</a> 👨‍👩‍👧</p>";
        let block = cf_html(fragment);
        assert_eq!(parse_cf_html(&block), Ok(fragment));
        // The character count is not the byte count.
        assert_ne!(fragment.chars().count(), fragment.len());
    }

    #[test]
    fn hane_clipboard_cf_html_accepts_lf_headers_and_missing_context() {
        let fragment = "<b>x</b>";
        let text = format!(
            "Version:1.0\nStartHTML:-1\nEndHTML:-1\nStartFragment:0000000084\nEndFragment:{:010}\n{fragment}",
            84 + fragment.len()
        );
        let header_len = text.len() - fragment.len();
        assert_eq!(header_len, 84, "the fixture's own offsets must be exact");
        assert_eq!(parse_cf_html(text.as_bytes()), Ok(fragment));
    }

    #[test]
    fn hane_clipboard_cf_html_rejects_bad_offsets_without_searching_markers() {
        let fragment = "<p>日本</p>";
        let good = String::from_utf8(cf_html(fragment)).unwrap();
        let end = good.len();
        let replace = |from: &str, to: String| good.replacen(from, &to, 1).into_bytes();

        let sf = good.find("<p>日本</p>").unwrap();
        let ef = sf + fragment.len();
        let sf_text = format!("StartFragment:{sf:010}");
        let ef_text = format!("EndFragment:{ef:010}");

        // End past the allocation.
        let bytes = replace(&ef_text, format!("EndFragment:{:010}", end + 10));
        assert_eq!(parse_cf_html(&bytes), Err(RichClipboardError::InvalidFormat));
        // Start after end.
        let bytes = replace(&sf_text, format!("StartFragment:{:010}", ef + 1));
        assert_eq!(parse_cf_html(&bytes), Err(RichClipboardError::InvalidFormat));
        // Start inside the header.
        let bytes = replace(&sf_text, format!("StartFragment:{:010}", 3));
        assert_eq!(parse_cf_html(&bytes), Err(RichClipboardError::InvalidFormat));
        // A negative fragment offset.
        let bytes = replace(&sf_text, "StartFragment:-000001".to_owned());
        assert_eq!(parse_cf_html(&bytes), Err(RichClipboardError::InvalidFormat));
        // The range cuts a multi-byte character.
        let bytes = replace(&sf_text, format!("StartFragment:{:010}", sf + 4));
        assert_eq!(parse_cf_html(&bytes), Err(RichClipboardError::InvalidFormat));
        // The context does not contain the fragment.
        let bytes = replace("EndHTML:", "EndHTML:9".to_owned());
        assert!(parse_cf_html(&bytes).is_err());
        // A missing header.
        assert_eq!(
            parse_cf_html(b"<p>no header</p>"),
            Err(RichClipboardError::InvalidFormat)
        );
        assert_eq!(parse_cf_html(b""), Err(RichClipboardError::InvalidFormat));
    }

    #[test]
    fn hane_clipboard_cf_html_rejects_html_offsets_below_minus_one() {
        let good = String::from_utf8(cf_html("<b>x</b>")).unwrap();
        let start = good.find("StartHTML:").unwrap() + "StartHTML:".len();
        let mut broken = good.clone();
        broken.replace_range(start..start + 10, "-000000002");
        assert_eq!(
            parse_cf_html(broken.as_bytes()),
            Err(RichClipboardError::InvalidFormat)
        );
    }

    #[test]
    fn hane_clipboard_cf_html_header_scan_stops_at_the_fragment_offsets() {
        // A fragment whose first line looks like a header line is still content.
        let fragment = "Note:x\r\n<b>y</b>";
        let block = cf_html(fragment);
        assert_eq!(parse_cf_html(&block), Ok(fragment));
    }
}

struct LockedGlobal {
    global: HGLOBAL,
    ptr: *const u8,
    size: usize,
}

impl LockedGlobal {
    fn lock(global: HGLOBAL) -> Option<Self> {
        let size = unsafe { GlobalSize(global) };
        let ptr = unsafe { GlobalLock(global) };
        if ptr.is_null() {
            return None;
        }
        Some(Self {
            global,
            ptr: ptr as *const u8,
            size,
        })
    }

    fn as_bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.size) }
    }
}

impl Drop for LockedGlobal {
    fn drop(&mut self) {
        unsafe { GlobalUnlock(self.global).ok() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn hane_clipboard_format_priority_prefers_alpha_png_then_dib_then_svg() {
        let png = image_format_priority(*CLIPBOARD_PNG_FORMAT);
        let jpeg = image_format_priority(*CLIPBOARD_JPG_FORMAT);
        let gif = image_format_priority(*CLIPBOARD_GIF_FORMAT);
        let dib = image_format_priority(CF_DIB.0 as u32);
        let svg = image_format_priority(*CLIPBOARD_SVG_FORMAT);
        assert!(png > jpeg && png > gif);
        assert!(jpeg > dib && gif > dib);
        assert!(dib > svg);
    }

    #[test]
    fn hane_clipboard_truncated_dib_is_not_a_bitmap() {
        assert!(convert_dib_to_bmp(&[0u8; 10]).is_none());
        // header_size が不正、または画素開始位置が入力外のものは拒否する。
        assert!(convert_dib_to_bmp(&[0u8; 40]).is_none());
        let mut palette_header = [0u8; 40];
        palette_header[0..4].copy_from_slice(&40u32.to_le_bytes());
        palette_header[14..16].copy_from_slice(&8u16.to_le_bytes());
        assert!(convert_dib_to_bmp(&palette_header).is_none());
    }

    const MASK_R: u32 = 0x00FF_0000;
    const MASK_G: u32 = 0x0000_FF00;
    const MASK_B: u32 = 0x0000_00FF;
    const MASK_A: u32 = 0xFF00_0000;

    /// 2x1, 32bpp, BI_BITFIELDS の DIB を作る。`header_size` は 40 / 108 / 124。
    fn bitfields_dib(header_size: u32) -> Vec<u8> {
        let mut dib = Vec::new();
        dib.extend_from_slice(&header_size.to_le_bytes());
        dib.extend_from_slice(&2i32.to_le_bytes()); // width
        dib.extend_from_slice(&1i32.to_le_bytes()); // height
        dib.extend_from_slice(&1u16.to_le_bytes()); // planes
        dib.extend_from_slice(&32u16.to_le_bytes()); // bit count
        dib.extend_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
        dib.extend_from_slice(&8u32.to_le_bytes()); // image size
        dib.extend_from_slice(&[0u8; 16]); // ppm x/y, colors used/important
        assert_eq!(dib.len(), 40);
        if header_size == 40 {
            // ヘッダー外の RGB masks
            for mask in [MASK_R, MASK_G, MASK_B] {
                dib.extend_from_slice(&mask.to_le_bytes());
            }
        } else {
            // V4 / V5: ヘッダー内の RGBA masks
            for mask in [MASK_R, MASK_G, MASK_B, MASK_A] {
                dib.extend_from_slice(&mask.to_le_bytes());
            }
            dib.resize(header_size as usize, 0);
        }
        // BGRA 画素 2 つ: (R,G,B,A) = (0x10,0x20,0x30,0x80), (0xA0,0xB0,0xC0,0xFF)
        dib.extend_from_slice(&[0x30, 0x20, 0x10, 0x80]);
        dib.extend_from_slice(&[0xC0, 0xB0, 0xA0, 0xFF]);
        dib
    }

    fn decode_bmp(bmp: &[u8]) -> image::RgbaImage {
        image::load_from_memory_with_format(bmp, image::ImageFormat::Bmp)
            .expect("BMP decodes")
            .to_rgba8()
    }

    #[test]
    fn hane_clipboard_dib_info_header_bitfields_masks_are_outside_header() {
        let dib = bitfields_dib(40);
        let bmp = convert_dib_to_bmp(&dib).expect("converts");
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 66);
        let img = decode_bmp(&bmp);
        assert_eq!((img.width(), img.height()), (2, 1));
        assert_eq!(img.get_pixel(0, 0).0[..3], [0x10, 0x20, 0x30]);
        assert_eq!(img.get_pixel(1, 0).0[..3], [0xA0, 0xB0, 0xC0]);
    }

    #[test]
    fn hane_clipboard_dib_v4_v5_bitfields_masks_are_inside_header() {
        for (header_size, expected_offset) in [(108u32, 122u32), (124, 138)] {
            let dib = bitfields_dib(header_size);
            let bmp = convert_dib_to_bmp(&dib).expect("converts");
            assert_eq!(
                u32::from_le_bytes(bmp[10..14].try_into().unwrap()),
                expected_offset
            );
            let img = decode_bmp(&bmp);
            assert_eq!((img.width(), img.height()), (2, 1));
            assert_eq!(img.get_pixel(0, 0).0, [0x10, 0x20, 0x30, 0x80]);
            assert_eq!(img.get_pixel(1, 0).0, [0xA0, 0xB0, 0xC0, 0xFF]);
        }
    }

    #[test]
    fn hane_clipboard_dib_header_larger_than_input_is_rejected() {
        let mut dib = bitfields_dib(108);
        dib.truncate(60);
        assert!(convert_dib_to_bmp(&dib).is_none());
    }
}
