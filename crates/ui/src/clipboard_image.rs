//! Clipboard image paste (Issue #591): the parts that need no GPUI state.
//!
//! * decoding the OS image and re-encoding it as PNG, within finite limits;
//! * exclusive creation of `<document directory>/images/<generated>.png`;
//! * the shape (surrounding line breaks) of a stand-alone image line;
//! * the narrow rule that recognises a *generated* image reference again after
//!   a reload, and the copy of those images for a Save As to another directory.
//!
//! Nothing here keeps a manifest or any other persistent state: a generated
//! image is recognised only by its reference text (`images/hane-paste-…png`).

use std::collections::hash_map::RandomState;
use std::fmt;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Cursor, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use image::codecs::png::PngEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits};

/// Directory next to the document that receives pasted images.
pub(crate) const IMAGES_DIRECTORY: &str = "images";
pub(crate) const IMAGE_ALT_TEXT: &str = "画像";

/// Largest clipboard image payload accepted, in bytes (64 MiB). A 4K
/// screenshot is well below 10 MiB as PNG, an uncompressed 8K bitmap is
/// 128 MiB and is refused instead of being copied into memory.
pub(crate) const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
/// Largest accepted width or height. This is the decoder's own `Limits`
/// dimension check, so an oversized header is refused before any allocation.
pub(crate) const MAX_IMAGE_SIDE: u32 = 16_384;
/// Largest accepted decoded pixel count (40 megapixels: 8K × 5K). At 16 bits
/// per channel and four channels this is 320 MiB, under the allocation cap.
pub(crate) const MAX_IMAGE_PIXELS: u64 = 40_000_000;
/// The decoder's allocation cap (512 MiB), enforced by `Limits::max_alloc`.
pub(crate) const MAX_DECODE_ALLOC_BYTES: u64 = 512 * 1024 * 1024;
/// Largest PNG written (64 MiB), counted while encoding so a pathological
/// image stops early instead of filling the disk.
pub(crate) const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

const GENERATED_PREFIX: &str = "hane-paste-";
const GENERATED_SUFFIX: &str = ".png";
const NAME_ATTEMPTS: usize = 32;

#[derive(Debug)]
pub(crate) enum ImagePasteError {
    /// A finite limit was exceeded; the text names which one.
    TooLarge(&'static str),
    Decode(String),
    Encode(String),
    /// `images` exists but is not a directory.
    ImagesNotDirectory,
    /// Every generated name already existed.
    NameExhausted,
    /// A generated image the document refers to no longer exists, so it
    /// cannot be copied.
    SourceMissing,
    Io {
        action: &'static str,
        source: io::Error,
    },
}

impl ImagePasteError {
    fn io(action: &'static str, source: io::Error) -> Self {
        Self::Io { action, source }
    }
}

fn io_reason(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "権限がありません".to_owned(),
        io::ErrorKind::StorageFull => "ディスクの空き容量が足りません".to_owned(),
        io::ErrorKind::NotFound => "保存先のフォルダーが見つかりません".to_owned(),
        _ => error.to_string(),
    }
}

impl fmt::Display for ImagePasteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge(what) => write!(formatter, "画像が大きすぎます（{what}）"),
            Self::Decode(detail) => write!(formatter, "画像を読み取れません: {detail}"),
            Self::Encode(detail) => write!(formatter, "画像をPNGに変換できません: {detail}"),
            Self::ImagesNotDirectory => {
                formatter.write_str("images がフォルダーではないため画像を保存できません")
            }
            Self::NameExhausted => formatter.write_str("画像のファイル名を決められませんでした"),
            Self::SourceMissing => {
                formatter.write_str("コピーする元の画像が見つかりません")
            }
            Self::Io { action, source } => write!(formatter, "{action}: {}", io_reason(source)),
        }
    }
}

/// The raster formats this paste decodes. SVG, ICO and PNM are not accepted as
/// "a real image": the first is not a raster and the others are not what a
/// screenshot or image app puts on the clipboard.
pub(crate) fn source_format(format: gpui::ImageFormat) -> Option<ImageFormat> {
    match format {
        gpui::ImageFormat::Png => Some(ImageFormat::Png),
        gpui::ImageFormat::Jpeg => Some(ImageFormat::Jpeg),
        gpui::ImageFormat::Webp => Some(ImageFormat::WebP),
        gpui::ImageFormat::Gif => Some(ImageFormat::Gif),
        gpui::ImageFormat::Bmp => Some(ImageFormat::Bmp),
        gpui::ImageFormat::Tiff => Some(ImageFormat::Tiff),
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) struct EncodedPng {
    pub(crate) bytes: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

fn map_image_error(error: ImageError) -> ImagePasteError {
    match error {
        ImageError::Limits(_) => ImagePasteError::TooLarge("寸法またはメモリ使用量"),
        other => ImagePasteError::Decode(other.to_string()),
    }
}

/// A `Write` that refuses to grow past `limit`.
struct CappedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl Write for CappedBuffer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(buffer.len()) > self.limit {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "PNG output limit exceeded",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Decodes `bytes` and re-encodes them as PNG. Alpha is kept (the PNG encoder
/// writes the decoded colour type as is) and the EXIF orientation is applied to
/// the pixels, so the PNG — which carries no orientation — shows the same way
/// up as the source did.
pub(crate) fn encode_clipboard_image(
    bytes: &[u8],
    format: ImageFormat,
) -> Result<EncodedPng, ImagePasteError> {
    encode_with_limits(bytes, format, MAX_OUTPUT_BYTES)
}

#[allow(
    clippy::field_reassign_with_default,
    reason = "`Limits` is non-exhaustive, so it cannot be built with a struct literal"
)]
fn encode_with_limits(
    bytes: &[u8],
    format: ImageFormat,
    output_limit: usize,
) -> Result<EncodedPng, ImagePasteError> {
    if bytes.is_empty() {
        return Err(ImagePasteError::Decode("画像データが空です".to_owned()));
    }
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(ImagePasteError::TooLarge("データサイズ"));
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(map_image_error)?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(ImagePasteError::Decode("画像の寸法が0です".to_owned()));
    }
    if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return Err(ImagePasteError::TooLarge("幅または高さ"));
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(ImagePasteError::TooLarge("画素数"));
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(Orientation::NoTransforms);
    let mut decoded = DynamicImage::from_decoder(decoder).map_err(map_image_error)?;
    decoded.apply_orientation(orientation);
    let (width, height) = (decoded.width(), decoded.height());

    let mut output = CappedBuffer {
        bytes: Vec::new(),
        limit: output_limit,
        exceeded: false,
    };
    let written = decoded.write_with_encoder(PngEncoder::new(&mut output));
    if output.exceeded {
        return Err(ImagePasteError::TooLarge("PNGのサイズ"));
    }
    written.map_err(|error| ImagePasteError::Encode(error.to_string()))?;
    Ok(EncodedPng {
        bytes: output.bytes,
        width,
        height,
    })
}

/// A new `hane-paste-<unix millis>-<8 hex>.png`. The name is only a candidate:
/// uniqueness comes from the exclusive create, never from this value.
pub(crate) fn generated_file_name() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(millis);
    hasher.write_u64(SEQUENCE.fetch_add(1, Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    let tag = hasher.finish() & 0xffff_ffff;
    format!("{GENERATED_PREFIX}{millis}-{tag:08x}{GENERATED_SUFFIX}")
}

/// The narrow rule that says "Hane generated this file name": the exact shape
/// `generated_file_name` produces. Anything else (hand-written references,
/// other tools' files) is not managed.
pub(crate) fn is_generated_file_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(GENERATED_PREFIX)
        .and_then(|rest| rest.strip_suffix(GENERATED_SUFFIX))
    else {
        return false;
    };
    let Some((millis, tag)) = rest.split_once('-') else {
        return false;
    };
    (1..=39).contains(&millis.len())
        && millis.bytes().all(|byte| byte.is_ascii_digit())
        && tag.len() == 8
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The Markdown destination for a generated file name.
pub(crate) fn image_destination(file_name: &str) -> String {
    format!("{IMAGES_DIRECTORY}/{file_name}")
}

pub(crate) fn image_reference(file_name: &str) -> String {
    format!("![{IMAGE_ALT_TEXT}]({})", image_destination(file_name))
}

#[derive(Debug)]
pub(crate) struct SavedImage {
    pub(crate) path: PathBuf,
    pub(crate) file_name: String,
}

/// Creates `<base>/images` when missing. `base` itself must exist: a document
/// directory that was deleted is not silently recreated.
fn ensure_images_directory(base: &Path) -> Result<PathBuf, ImagePasteError> {
    let directory = base.join(IMAGES_DIRECTORY);
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            match fs::metadata(&directory) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(ImagePasteError::ImagesNotDirectory),
                Err(error) => {
                    return Err(ImagePasteError::io("images フォルダーを確認できません", error));
                }
            }
        }
        Err(error) => {
            return Err(ImagePasteError::io("images フォルダーを作成できません", error));
        }
    }
    Ok(directory)
}

/// Writes `contents` to a new file in `directory`, never over an existing one:
/// `create_new` fails on an existing name (or a dangling symlink) and the next
/// candidate is tried. A write that fails part way removes the file this call
/// created — and only that file.
fn create_exclusive(
    directory: &Path,
    contents: &[u8],
    next_name: &mut dyn FnMut() -> String,
    action: &'static str,
) -> Result<SavedImage, ImagePasteError> {
    for _ in 0..NAME_ATTEMPTS {
        let file_name = next_name();
        let path = directory.join(&file_name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let written = file.write_all(contents).and_then(|()| file.sync_all());
                drop(file);
                return match written {
                    Ok(()) => Ok(SavedImage { path, file_name }),
                    Err(error) => {
                        let _ = fs::remove_file(&path);
                        Err(ImagePasteError::io(action, error))
                    }
                };
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(ImagePasteError::io(action, error)),
        }
    }
    Err(ImagePasteError::NameExhausted)
}

/// Saves `png` as a new file under `<base>/images`.
pub(crate) fn save_png(base: &Path, png: &[u8]) -> Result<SavedImage, ImagePasteError> {
    save_png_with(base, png, &mut generated_file_name)
}

pub(crate) fn save_png_with(
    base: &Path,
    png: &[u8],
    next_name: &mut dyn FnMut() -> String,
) -> Result<SavedImage, ImagePasteError> {
    let directory = ensure_images_directory(base)?;
    create_exclusive(&directory, png, next_name, "画像を保存できません")
}

/// A generated reference found in document text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GeneratedReference {
    pub(crate) file_name: String,
    /// Byte range of the file name inside the text (after `images/`).
    pub(crate) name_range: Range<usize>,
}

/// Every Markdown image `![..](images/<generated name>)` in `text`, in document
/// order. Which text is an image comes from the Markdown parser, so code
/// blocks, inline code, escaped text and plain links are never references.
pub(crate) fn generated_references(text: &str) -> Vec<GeneratedReference> {
    const OPENER: &str = "](images/";
    if !text.contains(OPENER) {
        return Vec::new();
    }
    let parsed = hane_markdown::parse_document(
        hane_document::Revision(0),
        hane_document::SourceRange::new(0, text.len()),
        text,
    );
    let mut found = Vec::new();
    for (_, node) in parsed.tree.iter() {
        if node.kind != hane_markdown::NodeKind::Image {
            continue;
        }
        let (image_start, image_end) = (node.source_range.start.0, node.source_range.end.0);
        let Some(source) = text.get(image_start..image_end) else {
            continue;
        };
        // The destination is the last `](images/` of the image, and the name
        // must be followed by the closing paren that ends the image.
        let Some(index) = source.rfind(OPENER) else {
            continue;
        };
        let name_start = index + OPENER.len();
        let Some(candidate) = source[name_start..].strip_suffix(')') else {
            continue;
        };
        if is_generated_file_name(candidate) {
            found.push(GeneratedReference {
                file_name: candidate.to_owned(),
                name_range: image_start + name_start..image_start + name_start + candidate.len(),
            });
        }
    }
    found.sort_by_key(|reference| reference.name_range.start);
    found
}

#[derive(Debug)]
pub(crate) struct CopiedImages {
    /// `(old name, new name)` for images whose name was taken by a different
    /// file in the destination.
    pub(crate) renamed: Vec<(String, String)>,
    /// Files this copy created (not reused ones); the caller removes them if
    /// the Save As is abandoned before the document is saved.
    pub(crate) created: Vec<PathBuf>,
}

/// Copies the generated images `names` from `<from_base>/images` into
/// `<to_base>/images` for a Save As. The source is never touched. A name that
/// already exists in the destination is reused when the bytes are identical
/// and otherwise replaced by a new name (reported in `renamed`). A source file
/// that no longer exists fails the copy, so the destination never ends up
/// referring to a different image of the same name. Any failure removes what
/// this call created and is returned.
pub(crate) fn copy_generated_images(
    from_base: &Path,
    to_base: &Path,
    names: &[String],
    next_name: &mut dyn FnMut() -> String,
) -> Result<CopiedImages, ImagePasteError> {
    copy_generated_images_within(from_base, to_base, names, next_name, MAX_OUTPUT_BYTES)
}

fn copy_generated_images_within(
    from_base: &Path,
    to_base: &Path,
    names: &[String],
    next_name: &mut dyn FnMut() -> String,
    limit: usize,
) -> Result<CopiedImages, ImagePasteError> {
    let mut outcome = CopiedImages {
        renamed: Vec::new(),
        created: Vec::new(),
    };
    if from_base == to_base || names.is_empty() {
        return Ok(outcome);
    }
    let result = copy_each(from_base, to_base, names, next_name, &mut outcome, limit);
    if let Err(error) = result {
        for path in outcome.created.drain(..) {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }
    Ok(outcome)
}

/// Reads at most `limit + 1` bytes, so a result longer than `limit` shows the
/// file exceeds it without ever reading the rest.
fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn copy_each(
    from_base: &Path,
    to_base: &Path,
    names: &[String],
    next_name: &mut dyn FnMut() -> String,
    outcome: &mut CopiedImages,
    limit: usize,
) -> Result<(), ImagePasteError> {
    let source_directory = from_base.join(IMAGES_DIRECTORY);
    let mut destination_directory: Option<PathBuf> = None;
    let mut seen: Vec<&str> = Vec::new();
    for name in names {
        if seen.contains(&name.as_str()) {
            continue;
        }
        seen.push(name);
        let source = source_directory.join(name);
        let length = match fs::metadata(&source) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ImagePasteError::SourceMissing);
            }
            Err(error) => return Err(ImagePasteError::io("元の画像を確認できません", error)),
        };
        if length > limit as u64 {
            return Err(ImagePasteError::TooLarge("コピーする画像のサイズ"));
        }
        // The metadata only rejects early; the read itself is bounded because
        // the file may have grown since.
        let bytes = match read_bounded(&source, limit) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ImagePasteError::SourceMissing);
            }
            Err(error) => return Err(ImagePasteError::io("元の画像を読み取れません", error)),
        };
        if bytes.len() > limit {
            return Err(ImagePasteError::TooLarge("コピーする画像のサイズ"));
        }
        let directory = match &destination_directory {
            Some(directory) => directory.clone(),
            None => {
                let directory = ensure_images_directory(to_base)?;
                destination_directory = Some(directory.clone());
                directory
            }
        };
        // An occupied name is reused only when it already holds the same
        // bytes; otherwise the original name is tried first and then fresh
        // ones, always through the exclusive create.
        // The comparison read is bounded too: an existing file over the limit
        // is never the same content and is left untouched.
        if read_bounded(&directory.join(name), limit)
            .is_ok_and(|existing| existing.len() <= limit && existing == bytes)
        {
            continue;
        }
        let mut first = Some(name.clone());
        let mut candidates = || first.take().unwrap_or_else(&mut *next_name);
        let saved = create_exclusive(&directory, &bytes, &mut candidates, "画像をコピーできません")?;
        outcome.created.push(saved.path);
        if saved.file_name != *name {
            outcome.renamed.push((name.clone(), saved.file_name));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreviousLine {
    DocumentStart,
    Blank,
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NextLine {
    DocumentEnd,
    Blank,
    Text,
}

/// The text around the replaced range, on its first and last line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LineContext<'a> {
    /// From the start of the line to the start of the replaced range.
    pub(crate) prefix: &'a str,
    /// From the end of the replaced range to the end of that line's content.
    pub(crate) suffix: &'a str,
    pub(crate) previous: PreviousLine,
    pub(crate) next: NextLine,
    pub(crate) line_ending: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InsertionShape {
    pub(crate) leading: String,
    pub(crate) trailing: String,
}

impl InsertionShape {
    pub(crate) fn replacement(&self, reference: &str) -> String {
        format!("{}{reference}{}", self.leading, self.trailing)
    }
}

/// The line breaks that make the reference a stand-alone line of its own
/// paragraph, adding only the breaks that are missing.
pub(crate) fn plan_shape(context: LineContext<'_>) -> InsertionShape {
    let eol = context.line_ending;
    let leading = if context.prefix.is_empty() {
        match context.previous {
            PreviousLine::Text => eol.to_owned(),
            PreviousLine::DocumentStart | PreviousLine::Blank => String::new(),
        }
    } else if context.prefix.trim().is_empty() {
        eol.to_owned()
    } else {
        format!("{eol}{eol}")
    };
    let trailing = if context.suffix.trim().is_empty() {
        match context.next {
            NextLine::Blank => String::new(),
            NextLine::DocumentEnd | NextLine::Text => eol.to_owned(),
        }
    } else {
        format!("{eol}{eol}")
    };
    InsertionShape { leading, trailing }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn temp_root(label: &str) -> PathBuf {
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-image-paste-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn png_of(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(pixel));
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    fn gray_png(width: u32, height: u32) -> Vec<u8> {
        let image = image::GrayImage::new(width, height);
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn a_transparent_png_keeps_its_alpha() {
        let source = png_of(2, 3, [10, 20, 30, 0]);
        let encoded = encode_clipboard_image(&source, ImageFormat::Png).unwrap();
        assert_eq!((encoded.width, encoded.height), (2, 3));
        let decoded = image::load_from_memory_with_format(&encoded.bytes, ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.get_pixel(1, 2).0[3], 0);
        let translucent = png_of(1, 1, [1, 2, 3, 128]);
        let encoded = encode_clipboard_image(&translucent, ImageFormat::Png).unwrap();
        let decoded = image::load_from_memory(&encoded.bytes).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [1, 2, 3, 128]);
    }

    #[test]
    fn other_raster_formats_become_png() {
        let rgb = image::RgbImage::from_pixel(4, 2, image::Rgb([200, 100, 50]));
        for format in [ImageFormat::Jpeg, ImageFormat::Bmp, ImageFormat::Tiff, ImageFormat::Gif] {
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgb8(rgb.clone())
                .write_to(&mut Cursor::new(&mut bytes), format)
                .unwrap();
            let encoded = encode_clipboard_image(&bytes, format).unwrap();
            assert_eq!((encoded.width, encoded.height), (4, 2), "{format:?}");
            assert!(encoded.bytes.starts_with(&[0x89, b'P', b'N', b'G']));
        }
    }

    #[test]
    fn svg_ico_and_pnm_are_not_real_raster_images() {
        assert!(source_format(gpui::ImageFormat::Svg).is_none());
        assert!(source_format(gpui::ImageFormat::Ico).is_none());
        assert!(source_format(gpui::ImageFormat::Pnm).is_none());
        assert_eq!(source_format(gpui::ImageFormat::Png), Some(ImageFormat::Png));
    }

    #[test]
    fn corrupt_empty_and_mismatched_data_are_decode_errors() {
        assert!(matches!(
            encode_clipboard_image(&[], ImageFormat::Png),
            Err(ImagePasteError::Decode(_))
        ));
        assert!(matches!(
            encode_clipboard_image(b"not an image at all", ImageFormat::Png),
            Err(ImagePasteError::Decode(_))
        ));
        let mut truncated = png_of(8, 8, [1, 2, 3, 255]);
        truncated.truncate(truncated.len() / 2);
        assert!(encode_clipboard_image(&truncated, ImageFormat::Png).is_err());
    }

    #[test]
    fn the_input_size_limit_is_inclusive() {
        let over = vec![0_u8; MAX_INPUT_BYTES + 1];
        assert!(matches!(
            encode_clipboard_image(&over, ImageFormat::Png),
            Err(ImagePasteError::TooLarge(_))
        ));
        // At the limit the size check passes and the content is judged instead.
        let at_limit = vec![0_u8; MAX_INPUT_BYTES];
        assert!(matches!(
            encode_clipboard_image(&at_limit, ImageFormat::Png),
            Err(ImagePasteError::Decode(_))
        ));
    }

    #[test]
    fn the_side_limit_is_inclusive() {
        let at_limit = gray_png(MAX_IMAGE_SIDE, 1);
        assert!(encode_clipboard_image(&at_limit, ImageFormat::Png).is_ok());
        let over = gray_png(MAX_IMAGE_SIDE + 1, 1);
        assert!(matches!(
            encode_clipboard_image(&over, ImageFormat::Png),
            Err(ImagePasteError::TooLarge(_))
        ));
    }

    #[test]
    fn the_pixel_limit_is_checked_before_decoding() {
        // 7000 × 7000 = 49 M pixels: each side is allowed, the product is not.
        let over = gray_png(7_000, 7_000);
        assert!(matches!(
            encode_clipboard_image(&over, ImageFormat::Png),
            Err(ImagePasteError::TooLarge("画素数" | "寸法またはメモリ使用量"))
        ));
    }

    #[test]
    fn the_output_limit_stops_encoding() {
        let source = png_of(64, 64, [9, 9, 9, 255]);
        assert!(matches!(
            encode_with_limits(&source, ImageFormat::Png, 16),
            Err(ImagePasteError::TooLarge(_))
        ));
        assert!(encode_with_limits(&source, ImageFormat::Png, MAX_OUTPUT_BYTES).is_ok());
    }

    #[test]
    fn generated_names_have_the_recognised_shape() {
        for _ in 0..64 {
            let name = generated_file_name();
            assert!(name.is_ascii());
            assert!(is_generated_file_name(&name), "{name}");
        }
        assert_ne!(generated_file_name(), generated_file_name());
        for bad in [
            "photo.png",
            "hane-paste-.png",
            "hane-paste-1-abc.png",
            "hane-paste-1-ABCDEF01.png",
            "hane-paste-x-abcdef01.png",
            "hane-paste-1-abcdef01.jpg",
            "hane-paste-1-abcdef01.png/..",
            "../hane-paste-1-abcdef01.png",
        ] {
            assert!(!is_generated_file_name(bad), "{bad}");
        }
        assert!(is_generated_file_name("hane-paste-1730000000000-0123abcd.png"));
    }

    #[test]
    fn the_reference_is_a_plain_markdown_image() {
        assert_eq!(
            image_reference("hane-paste-1-0123abcd.png"),
            "![画像](images/hane-paste-1-0123abcd.png)"
        );
    }

    #[test]
    fn generated_references_are_found_again_in_reloaded_text() {
        let text = "intro\n\n![画像](images/hane-paste-1-0123abcd.png)\n\n\
                    ![手書き](images/photo.png) ![x](images/hane-paste-22-89abcdef.png)\n\
                    ![外](https://example.com/images/hane-paste-1-0123abcd.png)\n";
        let found = generated_references(text);
        let names: Vec<&str> = found.iter().map(|r| r.file_name.as_str()).collect();
        assert_eq!(
            names,
            ["hane-paste-1-0123abcd.png", "hane-paste-22-89abcdef.png"]
        );
        for reference in &found {
            assert_eq!(&text[reference.name_range.clone()], reference.file_name);
        }
    }

    #[test]
    fn generated_references_ignore_code_and_escaped_examples() {
        let name = "hane-paste-1-0123abcd.png";
        let image = format!("![x](images/{name})");
        let link = format!("[x](images/{name})");
        let escaped = format!("\\![x](images/{name})");
        for example in [
            format!("```\n{image}\n```\n"),
            format!("````md\n```\n{image}\n```\n````\n"),
            format!("~~~\n{image}\n~~~\n"),
            // An unclosed fence swallows the rest of the document.
            format!("```\n{image}\n"),
            format!("    {image}\n"),
            format!("`{image}`\n"),
            format!("``a ` {image} ``\n"),
            format!("{link}\n"),
            format!("{escaped}\n"),
        ] {
            assert!(generated_references(&example).is_empty(), "{example:?}");
        }
    }

    #[test]
    fn generated_references_keep_real_images_next_to_examples() {
        let first = "hane-paste-1-0123abcd.png";
        let second = "hane-paste-2-89abcdef.png";
        let text = format!(
            "日本語 `![e](images/{first})` 後\n\n```\n![e](images/{first})\n```\n\n\
             ![日本](images/{first})\n\n``x`![e](images/{second})`` ![語](images/{second})\n"
        );
        let found = generated_references(&text);
        let names: Vec<&str> = found.iter().map(|r| r.file_name.as_str()).collect();
        assert_eq!(names, [first, second]);
        let real_first = text.rfind(&format!("](images/{first})")).unwrap() + "](images/".len();
        assert_eq!(found[0].name_range, real_first..real_first + first.len());
        for reference in &found {
            assert_eq!(&text[reference.name_range.clone()], reference.file_name);
        }
    }

    #[test]
    fn saving_creates_images_next_to_the_document_and_never_overwrites() {
        let root = temp_root("save");
        let base = root.join("ノート 1 (copy)");
        fs::create_dir_all(&base).unwrap();
        let first = save_png(&base, b"first").unwrap();
        assert_eq!(first.path.parent().unwrap(), base.join("images"));
        assert_eq!(fs::read(&first.path).unwrap(), b"first");

        // The generator keeps proposing the taken name first.
        let mut candidates = vec![
            "hane-paste-3-00000003.png".to_owned(),
            first.file_name.clone(),
            first.file_name.clone(),
        ];
        let second = save_png_with(&base, b"second", &mut || candidates.pop().unwrap()).unwrap();
        assert_eq!(fs::read(&first.path).unwrap(), b"first");
        assert_ne!(second.file_name, first.file_name);
        assert_eq!(fs::read(&second.path).unwrap(), b"second");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_name_that_keeps_colliding_is_given_up() {
        let root = temp_root("exhausted");
        let first = save_png(&root, b"x").unwrap();
        let taken = first.file_name.clone();
        let result = save_png_with(&root, b"y", &mut || taken.clone());
        assert!(matches!(result, Err(ImagePasteError::NameExhausted)));
        assert_eq!(fs::read(&first.path).unwrap(), b"x");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn images_that_is_a_file_is_refused_without_touching_it() {
        let root = temp_root("images-file");
        fs::write(root.join("images"), b"keep").unwrap();
        let result = save_png(&root, b"png");
        assert!(matches!(result, Err(ImagePasteError::ImagesNotDirectory)));
        assert_eq!(fs::read(root.join("images")).unwrap(), b"keep");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_document_directory_is_not_recreated() {
        let root = temp_root("missing-base");
        let base = root.join("gone");
        let result = save_png(&base, b"png");
        assert!(matches!(result, Err(ImagePasteError::Io { .. })));
        assert!(!base.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_occupied_candidate_never_damages_existing_entries() {
        let root = temp_root("partial");
        let existing = save_png(&root, b"existing").unwrap();
        // A directory already holds the only candidate name.
        let blocker = root.join("images").join("hane-paste-9-00000009.png");
        fs::create_dir(&blocker).unwrap();
        let result = save_png_with(&root, b"new", &mut || "hane-paste-9-00000009.png".to_owned());
        assert!(result.is_err());
        assert_eq!(fs::read(&existing.path).unwrap(), b"existing");
        assert!(blocker.is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copying_for_save_as_reuses_identical_and_renames_conflicting_files() {
        let root = temp_root("copy");
        let (from, to) = (root.join("a"), root.join("b"));
        fs::create_dir_all(from.join("images")).unwrap();
        fs::create_dir_all(to.join("images")).unwrap();
        let same = "hane-paste-1-00000001.png".to_owned();
        let clash = "hane-paste-2-00000002.png".to_owned();
        let fresh = "hane-paste-3-00000003.png".to_owned();
        for (name, bytes) in [(&same, b"same"), (&clash, b"from"), (&fresh, b"new!")] {
            fs::write(from.join("images").join(name), bytes).unwrap();
        }
        fs::write(to.join("images").join(&same), b"same").unwrap();
        fs::write(to.join("images").join(&clash), b"other").unwrap();

        let names = vec![same.clone(), clash.clone(), fresh.clone(), same.clone()];
        let mut replacement = vec!["hane-paste-5-00000005.png".to_owned()];
        let copied = copy_generated_images(&from, &to, &names, &mut || replacement.remove(0))
            .unwrap();
        assert_eq!(
            copied.renamed,
            [(clash.clone(), "hane-paste-5-00000005.png".to_owned())]
        );
        assert_eq!(copied.created.len(), 2);
        assert_eq!(fs::read(to.join("images").join(&clash)).unwrap(), b"other");
        assert_eq!(
            fs::read(to.join("images").join("hane-paste-5-00000005.png")).unwrap(),
            b"from"
        );
        assert_eq!(fs::read(to.join("images").join(&fresh)).unwrap(), b"new!");
        // The source images stay.
        for name in [&same, &clash, &fresh] {
            assert!(from.join("images").join(name).is_file());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_copy_removes_what_it_created_and_keeps_the_source() {
        let root = temp_root("copy-fail");
        let (from, to) = (root.join("a"), root.join("b"));
        fs::create_dir_all(from.join("images")).unwrap();
        fs::create_dir_all(&to).unwrap();
        let first = "hane-paste-1-00000001.png".to_owned();
        let second = "hane-paste-2-00000002.png".to_owned();
        fs::write(from.join("images").join(&first), b"one").unwrap();
        // The second source is a directory, so reading it fails after the
        // first image was already copied.
        fs::create_dir(from.join("images").join(&second)).unwrap();
        let result = copy_generated_images(
            &from,
            &to,
            &[first.clone(), second.clone()],
            &mut generated_file_name,
        );
        assert!(result.is_err());
        assert!(!to.join("images").join(&first).exists());
        assert!(from.join("images").join(&first).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_source_fails_even_when_the_destination_has_the_same_name() {
        let root = temp_root("copy-missing");
        let (from, to) = (root.join("a"), root.join("b"));
        fs::create_dir_all(from.join("images")).unwrap();
        fs::create_dir_all(to.join("images")).unwrap();
        let first = "hane-paste-1-00000001.png".to_owned();
        let missing = "hane-paste-2-00000002.png".to_owned();
        fs::write(from.join("images").join(&first), b"one").unwrap();
        // A different image already sits under the missing name.
        fs::write(to.join("images").join(&missing), b"other").unwrap();

        let result = copy_generated_images(
            &from,
            &to,
            &[first.clone(), missing.clone()],
            &mut generated_file_name,
        );
        assert!(matches!(result, Err(ImagePasteError::SourceMissing)));
        // Only what this call created is removed; the existing file stays.
        assert!(!to.join("images").join(&first).exists());
        assert_eq!(fs::read(to.join("images").join(&missing)).unwrap(), b"other");
        assert!(from.join("images").join(&first).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_bounded_read_stops_one_byte_past_the_limit() {
        let root = temp_root("bounded-read");
        let path = root.join("file");
        fs::write(&path, b"abcdefgh").unwrap();
        assert_eq!(read_bounded(&path, 8).unwrap(), b"abcdefgh");
        assert_eq!(read_bounded(&path, 7).unwrap(), b"abcdefgh");
        assert_eq!(read_bounded(&path, 4).unwrap(), b"abcde");
        assert_eq!(read_bounded(&path, 0).unwrap(), b"a");
        fs::remove_dir_all(root).unwrap();
    }

    fn copy_with_limit(
        from: &Path,
        to: &Path,
        names: &[String],
        limit: usize,
    ) -> (Result<(), ImagePasteError>, CopiedImages) {
        let mut outcome = CopiedImages {
            renamed: Vec::new(),
            created: Vec::new(),
        };
        let mut fresh = || "hane-paste-9-00000009.png".to_owned();
        let result = copy_each(from, to, names, &mut fresh, &mut outcome, limit);
        (result, outcome)
    }

    #[test]
    fn a_source_at_the_limit_is_copied_and_one_byte_over_is_too_large() {
        let root = temp_root("copy-limit");
        let (from, to) = (root.join("a"), root.join("b"));
        fs::create_dir_all(from.join("images")).unwrap();
        fs::create_dir_all(&to).unwrap();
        let exact = "hane-paste-1-00000001.png".to_owned();
        let over = "hane-paste-2-00000002.png".to_owned();
        fs::write(from.join("images").join(&exact), b"12345678").unwrap();
        fs::write(from.join("images").join(&over), b"123456789").unwrap();

        let (result, outcome) = copy_with_limit(&from, &to, std::slice::from_ref(&exact), 8);
        assert!(result.is_ok());
        assert_eq!(outcome.created.len(), 1);
        assert_eq!(fs::read(to.join("images").join(&exact)).unwrap(), b"12345678");

        let (result, _) = copy_with_limit(&from, &to, std::slice::from_ref(&over), 8);
        assert!(matches!(result, Err(ImagePasteError::TooLarge(_))));
        assert!(!to.join("images").join(&over).exists());
        assert!(from.join("images").join(&over).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_existing_destination_over_the_limit_is_never_reused_or_changed() {
        let root = temp_root("copy-limit-existing");
        let (from, to) = (root.join("a"), root.join("b"));
        fs::create_dir_all(from.join("images")).unwrap();
        fs::create_dir_all(to.join("images")).unwrap();
        let name = "hane-paste-1-00000001.png".to_owned();
        // The destination starts with the source bytes but is longer than the
        // limit, so it must not count as identical.
        fs::write(from.join("images").join(&name), b"1234").unwrap();
        fs::write(to.join("images").join(&name), b"12345678").unwrap();

        let (result, outcome) = copy_with_limit(&from, &to, std::slice::from_ref(&name), 4);
        assert!(result.is_ok());
        assert_eq!(fs::read(to.join("images").join(&name)).unwrap(), b"12345678");
        assert_eq!(
            outcome.renamed,
            [(name.clone(), "hane-paste-9-00000009.png".to_owned())]
        );
        assert_eq!(
            fs::read(to.join("images").join("hane-paste-9-00000009.png")).unwrap(),
            b"1234"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_oversized_source_fails_the_copy_and_removes_only_what_it_created() {
        let root = temp_root("copy-limit-cleanup");
        let (from, to) = (root.join("a"), root.join("b"));
        fs::create_dir_all(from.join("images")).unwrap();
        fs::create_dir_all(to.join("images")).unwrap();
        let first = "hane-paste-1-00000001.png".to_owned();
        let big = "hane-paste-2-00000002.png".to_owned();
        let kept = "hane-paste-3-00000003.png".to_owned();
        fs::write(from.join("images").join(&first), b"one").unwrap();
        fs::write(from.join("images").join(&big), b"too large").unwrap();
        fs::write(to.join("images").join(&kept), b"existing").unwrap();

        let result = copy_generated_images_within(
            &from,
            &to,
            &[first.clone(), big.clone()],
            &mut generated_file_name,
            4,
        );
        assert!(matches!(result, Err(ImagePasteError::TooLarge(_))));
        assert!(!to.join("images").join(&first).exists());
        assert!(!to.join("images").join(&big).exists());
        assert_eq!(fs::read(to.join("images").join(&kept)).unwrap(), b"existing");
        assert_eq!(fs::read(from.join("images").join(&big)).unwrap(), b"too large");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copying_into_the_same_directory_does_nothing() {
        let root = temp_root("copy-same");
        let copied = copy_generated_images(
            &root,
            &root,
            &["hane-paste-1-00000001.png".to_owned()],
            &mut generated_file_name,
        )
        .unwrap();
        assert!(copied.renamed.is_empty() && copied.created.is_empty());
        assert!(!root.join("images").exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn shape(
        prefix: &str,
        suffix: &str,
        previous: PreviousLine,
        next: NextLine,
    ) -> InsertionShape {
        plan_shape(LineContext {
            prefix,
            suffix,
            previous,
            next,
            line_ending: "\n",
        })
    }

    #[test]
    fn an_empty_line_between_paragraphs_needs_no_extra_breaks() {
        let planned = shape("", "", PreviousLine::Blank, NextLine::Blank);
        assert_eq!(planned.replacement("R"), "R");
    }

    #[test]
    fn an_empty_document_gets_the_reference_and_a_line_end() {
        let planned = shape("", "", PreviousLine::DocumentStart, NextLine::DocumentEnd);
        assert_eq!(planned.replacement("R"), "R\n");
    }

    #[test]
    fn inside_a_line_the_reference_becomes_its_own_paragraph() {
        let planned = shape("before", "after", PreviousLine::Blank, NextLine::Blank);
        assert_eq!(planned.replacement("R"), "\n\nR\n\n");
    }

    #[test]
    fn at_a_line_start_under_text_a_blank_line_is_added_once() {
        let planned = shape("", "tail", PreviousLine::Text, NextLine::Blank);
        assert_eq!(planned.replacement("R"), "\nR\n\n");
        let planned = shape("", "", PreviousLine::Text, NextLine::Text);
        assert_eq!(planned.replacement("R"), "\nR\n");
    }

    #[test]
    fn the_documents_line_ending_is_used() {
        let planned = plan_shape(LineContext {
            prefix: "a",
            suffix: "",
            previous: PreviousLine::Blank,
            next: NextLine::DocumentEnd,
            line_ending: "\r\n",
        });
        assert_eq!(planned.replacement("R"), "\r\n\r\nR\r\n");
    }
}
