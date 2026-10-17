//! Size, count and time limits from the application-update implementation spec.

pub const MAX_API_JSON_BYTES: u64 = 1 << 20;
pub const MAX_CATALOG_BYTES: u64 = 256 << 10;
pub const MAX_SIGNATURE_BYTES: u64 = 16 << 10;
pub const MAX_INVENTORY_BYTES: u64 = 1 << 20;
pub const MAX_JOURNAL_BYTES: u64 = 1 << 20;
pub const MAX_ARCHIVE_BYTES: u64 = 256 << 20;
pub const MAX_UNPACKED_BYTES: u64 = 1 << 30;
pub const MAX_ENTRY_BYTES: u64 = 512 << 20;
pub const MAX_ENTRIES: usize = 10_000;
pub const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 16 << 20;
pub const MAX_PATH_BYTES: usize = 240;
pub const MAX_COMPONENT_BYTES: usize = 255;
/// Entries above `RATIO_CHECK_MIN_BYTES` that expand more than this are
/// treated as decompression bombs.
pub const MAX_COMPRESSION_RATIO: u64 = 500;
pub const RATIO_CHECK_MIN_BYTES: u64 = 1 << 20;

pub const MAX_REDIRECTS: usize = 5;
pub const CONNECT_TIMEOUT_MS: u64 = 10_000;
pub const METADATA_TOTAL_MS: u64 = 30_000;
pub const DOWNLOAD_TOTAL_MS: u64 = 15 * 60 * 1000;
pub const READ_STALL_MS: u64 = 30_000;

pub const MAX_CATALOG_VALIDITY_SECS: i64 = 180 * 24 * 60 * 60;
pub const MAX_FUTURE_SKEW_SECS: i64 = 24 * 60 * 60;

/// Free space reserved on each volume for the journal on top of the payload.
pub const JOURNAL_RESERVE_BYTES: u64 = 64 << 20;
