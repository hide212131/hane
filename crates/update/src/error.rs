use std::fmt;
use std::io;

/// Stable machine-readable failure classes. They are never collapsed into
/// "up to date": a 404, a rate limit or a bad signature each stay distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    NetworkUnavailable,
    Timeout,
    Truncated,
    Cancelled,
    RateLimited,
    NotFound,
    MetadataMissing,
    NotStable,
    DuplicateAsset,
    AssetMissing,
    AssetMismatch,
    TargetUnsupported,
    ProtocolUnsupported,
    StateIncompatible,
    SchemaUnsupported,
    MalformedMetadata,
    SignatureInvalid,
    TrustUnconfigured,
    IdentityMismatch,
    CatalogExpired,
    ClockInvalid,
    SequenceRegression,
    SequenceConflict,
    ReceiptUnwritable,
    NotApplicable,
    UnsupportedInstall,
    InstallModified,
    EndpointRejected,
    RedirectRejected,
    LimitExceeded,
    SizeMismatch,
    HashMismatch,
    InsufficientSpace,
    ArchiveRejected,
    StageNotEmpty,
    JournalInvalid,
    TransitionInvalid,
    StaleJournal,
    /// The running app executes from a read-only macOS App Translocation
    /// copy. Only the app layer reports it; placement detection itself keeps
    /// returning `UnsupportedInstall`.
    AppTranslocated,
    Io,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NetworkUnavailable => "network_unavailable",
            ErrorCode::Timeout => "timeout",
            ErrorCode::Truncated => "truncated",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::NotFound => "not_found",
            ErrorCode::MetadataMissing => "metadata_missing",
            ErrorCode::NotStable => "not_stable",
            ErrorCode::DuplicateAsset => "duplicate_asset",
            ErrorCode::AssetMissing => "asset_missing",
            ErrorCode::AssetMismatch => "asset_mismatch",
            ErrorCode::TargetUnsupported => "target_unsupported",
            ErrorCode::ProtocolUnsupported => "protocol_unsupported",
            ErrorCode::StateIncompatible => "state_incompatible",
            ErrorCode::SchemaUnsupported => "schema_unsupported",
            ErrorCode::MalformedMetadata => "malformed_metadata",
            ErrorCode::SignatureInvalid => "signature_invalid",
            ErrorCode::TrustUnconfigured => "trust_unconfigured",
            ErrorCode::IdentityMismatch => "identity_mismatch",
            ErrorCode::CatalogExpired => "catalog_expired",
            ErrorCode::ClockInvalid => "clock_invalid",
            ErrorCode::SequenceRegression => "sequence_regression",
            ErrorCode::SequenceConflict => "sequence_conflict",
            ErrorCode::ReceiptUnwritable => "receipt_unwritable",
            ErrorCode::NotApplicable => "not_applicable",
            ErrorCode::UnsupportedInstall => "unsupported_install",
            ErrorCode::InstallModified => "install_modified",
            ErrorCode::EndpointRejected => "endpoint_rejected",
            ErrorCode::RedirectRejected => "redirect_rejected",
            ErrorCode::LimitExceeded => "limit_exceeded",
            ErrorCode::SizeMismatch => "size_mismatch",
            ErrorCode::HashMismatch => "hash_mismatch",
            ErrorCode::InsufficientSpace => "insufficient_space",
            ErrorCode::ArchiveRejected => "archive_rejected",
            ErrorCode::StageNotEmpty => "stage_not_empty",
            ErrorCode::JournalInvalid => "journal_invalid",
            ErrorCode::TransitionInvalid => "transition_invalid",
            ErrorCode::StaleJournal => "stale_journal",
            ErrorCode::AppTranslocated => "app_translocated",
            ErrorCode::Io => "io",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateError {
    pub code: ErrorCode,
    pub detail: String,
}

impl UpdateError {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.detail)
    }
}

impl std::error::Error for UpdateError {}

impl From<io::Error> for UpdateError {
    fn from(err: io::Error) -> Self {
        let code = match err.kind() {
            io::ErrorKind::StorageFull => ErrorCode::InsufficientSpace,
            io::ErrorKind::TimedOut => ErrorCode::Timeout,
            _ => ErrorCode::Io,
        };
        UpdateError::new(code, err.to_string())
    }
}

pub type Result<T> = std::result::Result<T, UpdateError>;
