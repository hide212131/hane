//! GitHub "latest release" interpretation: stable-only, SemVer-ordered,
//! exact asset names, no fallback between targets.

use semver::Version;
use serde::Deserialize;

use crate::download::asset_url;
use crate::error::{ErrorCode, Result, UpdateError};
use crate::limits::MAX_API_JSON_BYTES;
use crate::model::{CATALOG_ASSET_NAME, SIGNATURE_ASSET_NAME, Target};
use crate::version::{VersionRelation, parse_release_tag, relation};

#[derive(Debug, Deserialize)]
struct RawRelease {
    id: u64,
    tag_name: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    browser_download_url: String,
}

/// The release the metadata fetch is pinned to. URLs are rebuilt from the
/// fixed tag and names; the API's own URLs are only compared, never followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseSelection {
    pub release_id: u64,
    pub tag: String,
    pub version: Version,
    pub relation: VersionRelation,
    pub archive_name: String,
    pub archive_size: u64,
    pub catalog_url: String,
    pub signature_url: String,
    pub archive_url: String,
}

fn unique<'a>(assets: &'a [RawAsset], name: &str) -> Result<Option<&'a RawAsset>> {
    let mut matches = assets.iter().filter(|asset| asset.name == name);
    let first = matches.next();
    if matches.next().is_some() {
        return Err(UpdateError::new(
            ErrorCode::DuplicateAsset,
            format!("release has several assets named {name:?}"),
        ));
    }
    Ok(first)
}

fn require_uploaded(asset: &RawAsset, tag: &str) -> Result<()> {
    if asset.state != "uploaded" {
        return Err(UpdateError::new(
            ErrorCode::AssetMissing,
            format!("asset {:?} is not fully uploaded", asset.name),
        ));
    }
    if asset.browser_download_url != asset_url(tag, &asset.name) {
        return Err(UpdateError::new(
            ErrorCode::AssetMismatch,
            format!("asset {:?} points outside the pinned release", asset.name),
        ));
    }
    Ok(())
}

/// Interprets the `releases/latest` JSON for `target`.
pub fn select_release(json: &[u8], current: &Version, target: Target) -> Result<ReleaseSelection> {
    if json.len() as u64 > MAX_API_JSON_BYTES {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "release JSON exceeds the size limit",
        ));
    }
    let raw: RawRelease = serde_json::from_slice(json).map_err(|err| {
        UpdateError::new(
            ErrorCode::MalformedMetadata,
            format!("invalid release JSON: {err}"),
        )
    })?;
    if raw.draft || raw.prerelease {
        return Err(UpdateError::new(
            ErrorCode::NotStable,
            "latest release is a draft or prerelease",
        ));
    }
    let version = parse_release_tag(&raw.tag_name)?;

    // Duplicates are rejected for every asset we depend on before looking for
    // absences, so a duplicated name is never mistaken for a missing one.
    let catalog = unique(&raw.assets, CATALOG_ASSET_NAME)?;
    let signature = unique(&raw.assets, SIGNATURE_ASSET_NAME)?;
    let archive = unique(&raw.assets, target.asset_name())?;
    let (Some(catalog), Some(signature)) = (catalog, signature) else {
        return Err(UpdateError::new(
            ErrorCode::MetadataMissing,
            "release has no signed update metadata",
        ));
    };
    let Some(archive) = archive else {
        return Err(UpdateError::new(
            ErrorCode::AssetMissing,
            format!("release has no {} asset", target.asset_name()),
        ));
    };
    for asset in [catalog, signature, archive] {
        require_uploaded(asset, &raw.tag_name)?;
    }
    Ok(ReleaseSelection {
        release_id: raw.id,
        relation: relation(current, &version),
        version,
        archive_name: archive.name.clone(),
        archive_size: archive.size,
        catalog_url: asset_url(&raw.tag_name, CATALOG_ASSET_NAME),
        signature_url: asset_url(&raw.tag_name, SIGNATURE_ASSET_NAME),
        archive_url: asset_url(&raw.tag_name, target.asset_name()),
        tag: raw.tag_name,
    })
}
