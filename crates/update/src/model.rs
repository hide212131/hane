use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, Result, UpdateError};

pub const REPOSITORY: &str = "hide212131/hane";
pub const CHANNEL_STABLE: &str = "stable";
pub const CATALOG_SCHEMA: u64 = 1;
pub const SIGNATURE_SCHEMA: u64 = 1;
pub const INVENTORY_SCHEMA: u64 = 1;
/// Helper protocol this build can interpret.
pub const UPDATER_PROTOCOL: u64 = 1;
pub const STATE_COMPATIBILITY_V1: &str = "backward-compatible-v1";

pub const CATALOG_ASSET_NAME: &str = "hane-update.json";
pub const SIGNATURE_ASSET_NAME: &str = "hane-update.json.sig";

/// Domain separators prepended to the raw signed bytes.
pub const CATALOG_DOMAIN: &[u8] = b"hane-update-catalog-v1\0";
pub const INVENTORY_DOMAIN: &[u8] = b"hane-install-v1\0";

/// The product's pinned macOS bundle identifier.
pub const PRODUCT_MACOS_BUNDLE_ID: &str = "io.github.hide212131.hane";
/// The fixed bundle identifier of the isolated `validation-feed` build
/// (Issue #510). It is recognised only when that feature is enabled; there is no
/// way to configure another identifier.
pub const VALIDATION_MACOS_BUNDLE_ID: &str = "io.github.hide212131.hane.validation";

/// The only bundle identifier this build accepts for the app, the helper and
/// candidate bundles. Standard builds accept the product identifier alone.
#[cfg(not(feature = "validation-feed"))]
pub const MACOS_BUNDLE_ID: &str = PRODUCT_MACOS_BUNDLE_ID;
#[cfg(feature = "validation-feed")]
pub const MACOS_BUNDLE_ID: &str = VALIDATION_MACOS_BUNDLE_ID;

pub const INVENTORY_FILE_NAME: &str = "hane-install.json";
pub const INVENTORY_SIGNATURE_FILE_NAME: &str = "hane-install.json.sig";

/// macOS layout 1 (v0.27.0 only): the signed inventory lived inside `Hane.app`
/// (paths relative to the bundle). Apple Silicon cannot run a bundle without a
/// valid code signature, and any file added inside a sealed bundle breaks that
/// seal, so layout 2 moved the inventory out. These paths are now read only to
/// recognise an already installed v0.27.0 bundle; a layout 2 archive or bundle
/// must not contain them.
pub const MACOS_LEGACY_INVENTORY_REL: &str = "Contents/Resources/hane-install.json";
pub const MACOS_LEGACY_INVENTORY_SIGNATURE_REL: &str = "Contents/Resources/hane-install.json.sig";
/// The only version that ever shipped layout 1.
pub const MACOS_LEGACY_VERSION: &str = "0.27.0";
pub const MACOS_LEGACY_LAYOUT_REVISION: u64 = 1;
/// The ad-hoc seal of a finished bundle; it is an ordinary inventoried file.
pub const MACOS_CODE_RESOURCES_REL: &str = "Contents/_CodeSignature/CodeResources";
/// The bundle's directory inside the macOS archive, with a trailing slash.
pub const MACOS_APP_PREFIX: &str = "hane-macos-arm64/Hane.app/";
/// macOS layout 2: the Ed25519 inventory and its signature sit next to the
/// bundle in `Hane.app.update/<version>-<source_sha>/`, never inside it.
pub const MACOS_ARCHIVE_ROOT: &str = "hane-macos-arm64";
pub const MACOS_SIDECAR_DIR: &str = "Hane.app.update";

/// Name of one sidecar generation: the release it describes.
pub fn macos_generation(version: &str, source_sha: &str) -> String {
    format!("{version}-{source_sha}")
}

/// Splits a generation name into version and source SHA. Only names made of
/// version characters and a 40-digit lowercase source SHA qualify, so a name
/// taken from an untrusted Info.plist can never act as a path.
pub fn parse_macos_generation(name: &str) -> Option<(&str, &str)> {
    let (version, source_sha) = name.rsplit_once('-')?;
    let version_ok = !version.is_empty()
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'));
    (version_ok && crate::util::is_lower_hex(source_sha, 40)).then_some((version, source_sha))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Target {
    MacosArm64,
    WindowsX64,
}

impl Target {
    pub const ALL: [Target; 2] = [Target::MacosArm64, Target::WindowsX64];

    pub fn triple(self) -> &'static str {
        match self {
            Target::MacosArm64 => "aarch64-apple-darwin",
            Target::WindowsX64 => "x86_64-pc-windows-msvc",
        }
    }

    pub fn asset_name(self) -> &'static str {
        match self {
            Target::MacosArm64 => "hane-macos-arm64.zip",
            Target::WindowsX64 => "hane-windows-x64.zip",
        }
    }

    /// ZIP layout revision the v1 protocol expects for this target. macOS is at
    /// revision 2: a sealed `Hane.app` plus an external sidecar inventory.
    pub fn layout_revision(self) -> u64 {
        match self {
            Target::MacosArm64 => 2,
            Target::WindowsX64 => 2,
        }
    }

    /// Exact triple match only: no CPU is mapped to a "close" asset.
    pub fn parse(triple: &str) -> Result<Target> {
        Target::ALL
            .into_iter()
            .find(|t| t.triple() == triple)
            .ok_or_else(|| {
                UpdateError::new(
                    ErrorCode::TargetUnsupported,
                    format!("unsupported target {triple:?}"),
                )
            })
    }

    /// The target this binary was built for, if it is one we distribute.
    pub fn current() -> Result<Target> {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Ok(Target::MacosArm64)
        } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Ok(Target::WindowsX64)
        } else {
            Err(UpdateError::new(
                ErrorCode::TargetUnsupported,
                "this OS/CPU has no update distribution",
            ))
        }
    }
}

/// Catalog schema 1, deserialized from the signed raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogDocument {
    pub schema: u64,
    pub channel: String,
    pub repository: String,
    pub version: String,
    pub tag: String,
    pub source_sha: String,
    pub catalog_sequence: u64,
    pub published_at: String,
    pub expires_at: String,
    pub updater_protocol: u64,
    pub min_updater_protocol: u64,
    pub state_compatibility: String,
    pub targets: BTreeMap<String, TargetEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetEntry {
    pub asset_name: String,
    pub archive_size: u64,
    pub archive_sha256: String,
    pub unpacked_size: u64,
    pub layout_revision: u64,
    /// SHA-256 of the raw signed inventory inside the ZIP (both targets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_sha256: Option<String>,
    /// macOS only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    /// No target carries a Team ID any more; a catalog that sets one is
    /// rejected. The field stays so such a catalog parses and fails explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_version: Option<String>,
}

/// Detached signature file: `{schema, signatures:[{key_id, signature_base64}]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureFile {
    pub schema: u64,
    pub signatures: Vec<SignatureEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureEntry {
    pub key_id: String,
    pub signature_base64: String,
}

#[cfg(test)]
mod bundle_id_tests {
    use super::*;

    #[test]
    fn the_validation_identity_is_distinct_and_fixed() {
        assert_ne!(PRODUCT_MACOS_BUNDLE_ID, VALIDATION_MACOS_BUNDLE_ID);
        assert_eq!(PRODUCT_MACOS_BUNDLE_ID, "io.github.hide212131.hane");
        assert_eq!(VALIDATION_MACOS_BUNDLE_ID, "io.github.hide212131.hane.validation");
    }

    #[cfg(not(feature = "validation-feed"))]
    #[test]
    fn a_standard_build_pins_the_product_identity_and_rejects_validation() {
        assert_eq!(MACOS_BUNDLE_ID, PRODUCT_MACOS_BUNDLE_ID);
        assert_ne!(MACOS_BUNDLE_ID, VALIDATION_MACOS_BUNDLE_ID);
    }

    #[cfg(feature = "validation-feed")]
    #[test]
    fn a_validation_build_pins_only_the_validation_identity() {
        assert_eq!(MACOS_BUNDLE_ID, VALIDATION_MACOS_BUNDLE_ID);
        assert_ne!(MACOS_BUNDLE_ID, PRODUCT_MACOS_BUNDLE_ID);
    }
}
