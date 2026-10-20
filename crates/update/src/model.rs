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

pub const MACOS_BUNDLE_ID: &str = "io.github.hide212131.hane";

pub const INVENTORY_FILE_NAME: &str = "hane-install.json";
pub const INVENTORY_SIGNATURE_FILE_NAME: &str = "hane-install.json.sig";

/// macOS: the signed inventory lives inside `Hane.app` (paths relative to the
/// bundle), so the installed bundle can be verified against the same Ed25519
/// update key as the catalog, without any Apple identity.
pub const MACOS_INVENTORY_REL: &str = "Contents/Resources/hane-install.json";
pub const MACOS_INVENTORY_SIGNATURE_REL: &str = "Contents/Resources/hane-install.json.sig";
/// The bundle's directory inside the macOS archive, with a trailing slash.
pub const MACOS_APP_PREFIX: &str = "hane-macos-arm64/Hane.app/";

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

    /// ZIP layout revision the v1 protocol expects for this target.
    pub fn layout_revision(self) -> u64 {
        match self {
            Target::MacosArm64 => 1,
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
