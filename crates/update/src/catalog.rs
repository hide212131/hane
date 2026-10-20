//! Schema 1 catalog verification.
//!
//! [`verify_catalog`] is the only way to obtain a [`VerifiedCandidate`]; there
//! is no constructor from unverified JSON, so later stages (staging, plans)
//! cannot be reached with unsigned data.

use semver::Version;

use crate::clock::Clock;
use crate::error::{ErrorCode, Result, UpdateError};
use crate::limits::{
    MAX_ARCHIVE_BYTES, MAX_CATALOG_BYTES, MAX_CATALOG_VALIDITY_SECS, MAX_FUTURE_SKEW_SECS,
    MAX_UNPACKED_BYTES,
};
use crate::model::{
    CATALOG_DOMAIN, CATALOG_SCHEMA, CHANNEL_STABLE, CatalogDocument, MACOS_BUNDLE_ID, REPOSITORY,
    STATE_COMPATIBILITY_V1, Target, TargetEntry, UPDATER_PROTOCOL,
};
use crate::release::ReleaseSelection;
use crate::trust::{SeenCatalog, SeenStore, TrustPolicy, verify_signature};
use crate::util::{
    is_lower_hex, is_sha256_hex, malformed, parse_rfc3339_utc, parse_strict_value, sha256_hex,
};
use crate::version::{VersionRelation, parse_current_version, parse_release_tag, relation};

/// Identifies one verified catalog for one target: derived from the SHA-256 of
/// the signed catalog bytes and the target triple.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CandidateId(String);

impl CandidateId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct VerifyContext<'a> {
    pub policy: &'a TrustPolicy,
    pub clock: &'a dyn Clock,
    pub current_version: &'a str,
    pub target: Target,
    pub seen: Option<&'a SeenCatalog>,
    /// Release tag the fetch was pinned to; the catalog must agree.
    pub expected_tag: Option<&'a str>,
}

/// A catalog whose signature, schema, protocol, target, time window and
/// sequence were all checked against one [`VerifyContext`].
#[derive(Debug, Clone)]
pub struct VerifiedCandidate {
    catalog: CatalogDocument,
    entry: TargetEntry,
    target: Target,
    version: Version,
    relation: VersionRelation,
    catalog_sha256: String,
    candidate_id: CandidateId,
    signing_key_id: String,
}

impl VerifiedCandidate {
    pub fn catalog(&self) -> &CatalogDocument {
        &self.catalog
    }

    pub fn target_entry(&self) -> &TargetEntry {
        &self.entry
    }

    pub fn target(&self) -> Target {
        self.target
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn relation(&self) -> VersionRelation {
        self.relation
    }

    pub fn catalog_sha256(&self) -> &str {
        &self.catalog_sha256
    }

    pub fn candidate_id(&self) -> &CandidateId {
        &self.candidate_id
    }

    pub fn signing_key_id(&self) -> &str {
        &self.signing_key_id
    }

    pub fn sequence(&self) -> u64 {
        self.catalog.catalog_sequence
    }

    /// Only a strictly newer version may be applied. Same and older versions
    /// are reported but never become applicable (no downgrade).
    pub fn is_applicable(&self) -> bool {
        self.relation == VersionRelation::Newer
    }
}

fn expired(detail: &str) -> UpdateError {
    UpdateError::new(ErrorCode::CatalogExpired, detail)
}

pub fn verify_catalog(
    catalog_bytes: &[u8],
    signature_bytes: &[u8],
    ctx: &VerifyContext<'_>,
) -> Result<VerifiedCandidate> {
    let current = parse_current_version(ctx.current_version)?;
    if catalog_bytes.len() as u64 > MAX_CATALOG_BYTES {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "catalog exceeds the size limit",
        ));
    }
    // Authenticate the exact bytes before interpreting any of them.
    let signing_key_id =
        verify_signature(ctx.policy, CATALOG_DOMAIN, catalog_bytes, signature_bytes)?;

    let value = parse_strict_value(catalog_bytes)?;
    if value.get("schema").and_then(serde_json::Value::as_u64) != Some(CATALOG_SCHEMA) {
        return Err(UpdateError::new(
            ErrorCode::SchemaUnsupported,
            "unknown catalog schema",
        ));
    }
    let catalog: CatalogDocument = serde_json::from_value(value)
        .map_err(|err| malformed(format!("invalid catalog: {err}")))?;

    if catalog.channel != CHANNEL_STABLE || catalog.repository != REPOSITORY {
        return Err(malformed("catalog channel or repository is not accepted"));
    }
    let version = crate::version::parse_stable_version(&catalog.version)?;
    let tag_version = parse_release_tag(&catalog.tag)?;
    if tag_version != version {
        return Err(malformed("catalog tag does not match its version"));
    }
    if let Some(expected) = ctx.expected_tag
        && expected != catalog.tag
    {
        return Err(UpdateError::new(
            ErrorCode::AssetMismatch,
            "catalog tag differs from the fetched release tag",
        ));
    }
    if !is_lower_hex(&catalog.source_sha, 40) {
        return Err(malformed("source_sha must be 40 lowercase hex digits"));
    }

    if catalog.min_updater_protocol > UPDATER_PROTOCOL
        || catalog.updater_protocol < UPDATER_PROTOCOL
        || catalog.min_updater_protocol > catalog.updater_protocol
    {
        return Err(UpdateError::new(
            ErrorCode::ProtocolUnsupported,
            format!(
                "release requires updater protocol {}..={}, this build speaks {UPDATER_PROTOCOL}",
                catalog.min_updater_protocol, catalog.updater_protocol
            ),
        ));
    }
    if catalog.state_compatibility != STATE_COMPATIBILITY_V1 {
        return Err(UpdateError::new(
            ErrorCode::StateIncompatible,
            "release is not backward-compatible with saved state",
        ));
    }

    check_time_window(&catalog, ctx.clock)?;

    let entry = catalog
        .targets
        .get(ctx.target.triple())
        .cloned()
        .ok_or_else(|| {
            UpdateError::new(
                ErrorCode::TargetUnsupported,
                format!("catalog has no entry for {}", ctx.target.triple()),
            )
        })?;
    check_target_entry(&entry, ctx.target, &version)?;

    let catalog_sha256 = sha256_hex(catalog_bytes);
    if let Some(seen) = ctx.seen {
        seen.validate()?;
        if catalog.catalog_sequence < seen.sequence {
            return Err(UpdateError::new(
                ErrorCode::SequenceRegression,
                format!(
                    "catalog sequence {} is below the stored {}",
                    catalog.catalog_sequence, seen.sequence
                ),
            ));
        }
        if catalog.catalog_sequence == seen.sequence && catalog_sha256 != seen.catalog_sha256 {
            return Err(UpdateError::new(
                ErrorCode::SequenceConflict,
                "same catalog sequence with different bytes",
            ));
        }
    }

    let candidate_id = CandidateId(sha256_hex(
        format!(
            "hane-candidate-v1\0{catalog_sha256}\0{}",
            ctx.target.triple()
        )
        .as_bytes(),
    ));
    Ok(VerifiedCandidate {
        relation: relation(&current, &version),
        catalog,
        entry,
        target: ctx.target,
        version,
        catalog_sha256,
        candidate_id,
        signing_key_id,
    })
}

fn check_time_window(catalog: &CatalogDocument, clock: &dyn Clock) -> Result<()> {
    let published = parse_rfc3339_utc(&catalog.published_at)
        .ok_or_else(|| malformed("published_at is not UTC RFC 3339"))?;
    let expires = parse_rfc3339_utc(&catalog.expires_at)
        .ok_or_else(|| malformed("expires_at is not UTC RFC 3339"))?;
    if expires <= published || expires - published > MAX_CATALOG_VALIDITY_SECS {
        return Err(malformed("catalog validity window is out of range"));
    }
    let now = clock.now_unix();
    if published > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
        return Err(UpdateError::new(
            ErrorCode::ClockInvalid,
            "catalog was published in the future; check the system clock",
        ));
    }
    if now >= expires {
        return Err(expired("catalog has expired"));
    }
    Ok(())
}

fn check_target_entry(
    entry: &TargetEntry,
    target: Target,
    version: &Version,
) -> Result<()> {
    if entry.asset_name != target.asset_name() {
        return Err(UpdateError::new(
            ErrorCode::AssetMismatch,
            "catalog asset name does not match the target",
        ));
    }
    if entry.archive_size == 0 || entry.archive_size > MAX_ARCHIVE_BYTES {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "archive_size is out of range",
        ));
    }
    if entry.unpacked_size == 0 || entry.unpacked_size > MAX_UNPACKED_BYTES {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "unpacked_size is out of range",
        ));
    }
    if !is_sha256_hex(&entry.archive_sha256) {
        return Err(malformed("archive_sha256 must be 64 lowercase hex digits"));
    }
    if entry.layout_revision != target.layout_revision() {
        return Err(UpdateError::new(
            ErrorCode::ProtocolUnsupported,
            "unsupported archive layout revision",
        ));
    }
    match target {
        Target::WindowsX64 => {
            match entry.inventory_sha256.as_deref() {
                Some(hash) if is_sha256_hex(hash) => {}
                _ => return Err(malformed("Windows target needs a valid inventory_sha256")),
            }
            if entry.bundle_id.is_some() || entry.team_id.is_some() || entry.bundle_version.is_some()
            {
                return Err(malformed("Windows target must not carry macOS identity"));
            }
        }
        Target::MacosArm64 => {
            match entry.inventory_sha256.as_deref() {
                Some(hash) if is_sha256_hex(hash) => {}
                _ => return Err(malformed("macOS target needs a valid inventory_sha256")),
            }
            if entry.bundle_id.as_deref() != Some(MACOS_BUNDLE_ID) {
                return Err(UpdateError::new(
                    ErrorCode::IdentityMismatch,
                    "bundle id differs from the pinned Hane bundle id",
                ));
            }
            // Trust comes from the Ed25519 update key only; no Apple identity
            // is part of the catalog.
            if entry.team_id.is_some() {
                return Err(malformed("macOS target must not carry a Team ID"));
            }
            let expected_bundle_version = version.to_string();
            if entry.bundle_version.as_deref() != Some(expected_bundle_version.as_str()) {
                return Err(UpdateError::new(
                    ErrorCode::IdentityMismatch,
                    "bundle version differs from the catalog version",
                ));
            }
        }
    }
    Ok(())
}

/// Checks that the release listing and the signed catalog describe the same
/// archive, so a mismatched API view cannot redirect the download.
pub fn confirm_selection(selection: &ReleaseSelection, candidate: &VerifiedCandidate) -> Result<()> {
    if selection.tag != candidate.catalog.tag || selection.version != candidate.version {
        return Err(UpdateError::new(
            ErrorCode::AssetMismatch,
            "release tag differs from the signed catalog",
        ));
    }
    if selection.archive_size != candidate.entry.archive_size {
        return Err(UpdateError::new(
            ErrorCode::AssetMismatch,
            "release asset size differs from the signed catalog",
        ));
    }
    Ok(())
}

/// Persists the sequence floor. A candidate may only be offered as Ready after
/// this returns `Ok`; a failure here means "not ready".
pub fn record_seen(store: &mut dyn SeenStore, candidate: &VerifiedCandidate) -> Result<()> {
    let unwritable = |err: UpdateError| {
        UpdateError::new(
            ErrorCode::ReceiptUnwritable,
            format!("cannot persist the catalog sequence: {err}"),
        )
    };
    // Held across load, compare and store (not the download) so a concurrent
    // process cannot lower the floor with a stale read.
    let _guard = store.lock().map_err(unwritable)?;
    let existing = store.load().map_err(unwritable)?;
    if let Some(existing) = &existing {
        existing.validate()?;
        if existing.sequence > candidate.sequence() {
            return Err(UpdateError::new(
                ErrorCode::SequenceRegression,
                "stored catalog sequence is newer",
            ));
        }
        if existing.sequence == candidate.sequence() && existing.catalog_sha256 != candidate.catalog_sha256 {
            return Err(UpdateError::new(
                ErrorCode::SequenceConflict,
                "same catalog sequence with different bytes",
            ));
        }
        if existing.sequence == candidate.sequence() {
            return Ok(());
        }
    }
    store
        .store(&SeenCatalog::new(
            candidate.sequence(),
            candidate.catalog_sha256.clone(),
        ))
        .map_err(unwritable)
}
