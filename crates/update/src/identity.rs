//! Install identity, signed inventory and the fixed-identity `ApplyPlan`.
//!
//! An [`ApplyPlan`] can only be built from a [`VerifiedCandidate`] and two
//! signature-verified inventories. It carries relative paths and hashes only:
//! no destination directory, command or script.

use std::collections::{BTreeMap, BTreeSet};

use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::catalog::VerifiedCandidate;
use crate::error::{ErrorCode, Result, UpdateError};
use crate::limits::{MAX_ENTRIES, MAX_ENTRY_BYTES, MAX_INVENTORY_BYTES};
use crate::model::{
    INVENTORY_DOMAIN, INVENTORY_FILE_NAME, INVENTORY_SCHEMA, INVENTORY_SIGNATURE_FILE_NAME,
    Target, UPDATER_PROTOCOL,
};
use crate::paths::{collision_key, validate_rel_path};
use crate::trust::{TrustPolicy, verify_signature};
use crate::util::{
    hex_encode, is_lower_hex, is_sha256_hex, malformed, parse_strict, parse_strict_value,
    sha256_context, sha256_hex,
};
use crate::version::{VersionRelation, relation};

/// 128-bit identifier of one installation location, rendered as 32 lowercase
/// hex digits. It names the coordination directory next to the install.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstallId(String);

/// Facts the OS adapter observes about an install. They are inputs only; the
/// adapter (a later stage) is responsible for obtaining them reliably.
pub struct InstallFacts<'a> {
    pub os: &'a str,
    pub volume_id: &'a str,
    pub install_path: &'a str,
    pub owner: &'a str,
}

impl InstallId {
    pub fn derive(facts: &InstallFacts<'_>) -> Result<Self> {
        let fields = [facts.os, facts.volume_id, facts.install_path, facts.owner];
        if fields.iter().any(|f| f.is_empty()) {
            return Err(UpdateError::new(
                ErrorCode::UnsupportedInstall,
                "install identity facts are incomplete",
            ));
        }
        let mut context = sha256_context();
        context.update(b"hane-install-id-v1\0");
        for field in fields {
            context.update(&(field.len() as u64).to_le_bytes());
            context.update(field.as_bytes());
        }
        let digest = hex_encode(context.finish().as_ref());
        Ok(Self(digest[..32].to_string()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        if is_lower_hex(text, 32) {
            Ok(Self(text.to_string()))
        } else {
            Err(UpdateError::new(
                ErrorCode::JournalInvalid,
                "install id must be 32 lowercase hex digits",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Name of the coordination directory placed beside the install.
    pub fn coordination_dir_name(&self) -> String {
        format!(".hane-update-{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransactionId(String);

impl TransactionId {
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 16];
        SystemRandom::new().fill(&mut bytes).map_err(|_| {
            UpdateError::new(ErrorCode::Io, "operating system randomness is unavailable")
        })?;
        Ok(Self(hex_encode(&bytes)))
    }

    pub fn parse(text: &str) -> Result<Self> {
        if is_lower_hex(text, 32) {
            Ok(Self(text.to_string()))
        } else {
            Err(UpdateError::new(
                ErrorCode::JournalInvalid,
                "transaction id must be 32 lowercase hex digits",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Size and SHA-256 of one file; the unit of comparison against the disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub size: u64,
    pub sha256: String,
}

impl FileIdentity {
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            size: bytes.len() as u64,
            sha256: sha256_hex(bytes),
        }
    }
}

/// Signed identity of a release on one side of an update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseIdentity {
    pub target: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
}

impl ReleaseIdentity {
    pub fn from_candidate(candidate: &VerifiedCandidate) -> Self {
        let entry = candidate.target_entry();
        Self {
            target: candidate.target().triple().to_string(),
            version: candidate.version().to_string(),
            catalog_sha256: Some(candidate.catalog_sha256().to_string()),
            inventory_sha256: entry.inventory_sha256.clone(),
            bundle_id: entry.bundle_id.clone(),
            team_id: entry.team_id.clone(),
        }
    }

    pub fn from_inventory(signed: &SignedInventory) -> Self {
        Self {
            target: signed.inventory.target.clone(),
            version: signed.inventory.version.clone(),
            catalog_sha256: None,
            inventory_sha256: Some(signed.raw_sha256.clone()),
            bundle_id: None,
            team_id: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        Target::parse(&self.target)?;
        crate::version::parse_stable_version(&self.version)?;
        for hash in [&self.catalog_sha256, &self.inventory_sha256]
            .into_iter()
            .flatten()
        {
            if !is_sha256_hex(hash) {
                return Err(UpdateError::new(
                    ErrorCode::JournalInvalid,
                    "identity hash is not SHA-256 hex",
                ));
            }
        }
        Ok(())
    }
}

/// `hane-install.json`: the files a Windows release owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub schema: u64,
    pub version: String,
    pub target: String,
    pub source_sha: String,
    pub updater_protocol: u64,
    pub layout_revision: u64,
    pub files: Vec<InventoryFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// An inventory whose signature verified against the embedded trust policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedInventory {
    inventory: Inventory,
    raw: FileIdentity,
    signature: FileIdentity,
    raw_sha256: String,
}

impl SignedInventory {
    pub fn verify(raw: &[u8], signature_file: &[u8], policy: &TrustPolicy) -> Result<Self> {
        if raw.len() as u64 > MAX_INVENTORY_BYTES {
            return Err(UpdateError::new(
                ErrorCode::LimitExceeded,
                "inventory exceeds the size limit",
            ));
        }
        verify_signature(policy, INVENTORY_DOMAIN, raw, signature_file)?;
        let probe = parse_strict_value(raw)?;
        if probe.get("schema").and_then(serde_json::Value::as_u64) != Some(INVENTORY_SCHEMA) {
            return Err(UpdateError::new(
                ErrorCode::SchemaUnsupported,
                "unknown inventory schema",
            ));
        }
        let inventory: Inventory = parse_strict(raw)?;
        inventory.validate()?;
        Ok(Self {
            raw_sha256: sha256_hex(raw),
            raw: FileIdentity::of(raw),
            signature: FileIdentity::of(signature_file),
            inventory,
        })
    }

    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }

    pub fn raw_sha256(&self) -> &str {
        &self.raw_sha256
    }

    /// The inventory must describe exactly the release the catalog announced.
    pub fn check_candidate(&self, candidate: &VerifiedCandidate) -> Result<()> {
        let inv = &self.inventory;
        let mismatch = |what: &str| {
            Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                format!("inventory {what} differs from the signed catalog"),
            ))
        };
        if inv.version != candidate.version().to_string() {
            return mismatch("version");
        }
        if inv.target != candidate.target().triple() {
            return mismatch("target");
        }
        if inv.source_sha != candidate.catalog().source_sha {
            return mismatch("source_sha");
        }
        if inv.updater_protocol != UPDATER_PROTOCOL {
            return Err(UpdateError::new(
                ErrorCode::ProtocolUnsupported,
                "inventory updater protocol is not supported",
            ));
        }
        if inv.layout_revision != candidate.target().layout_revision() {
            return mismatch("layout revision");
        }
        if candidate.target_entry().inventory_sha256.as_deref() != Some(self.raw_sha256.as_str()) {
            return Err(UpdateError::new(
                ErrorCode::HashMismatch,
                "inventory hash differs from the signed catalog",
            ));
        }
        Ok(())
    }
}

impl Inventory {
    fn validate(&self) -> Result<()> {
        Target::parse(&self.target)?;
        crate::version::parse_stable_version(&self.version)?;
        if !is_lower_hex(&self.source_sha, 40) {
            return Err(malformed("inventory source_sha must be 40 hex digits"));
        }
        if self.files.is_empty() || self.files.len() > MAX_ENTRIES {
            return Err(malformed("inventory file count out of range"));
        }
        let mut keys = BTreeSet::new();
        for file in &self.files {
            validate_rel_path(&file.path)?;
            if file.path == INVENTORY_FILE_NAME || file.path == INVENTORY_SIGNATURE_FILE_NAME {
                return Err(malformed("inventory must not list its own files"));
            }
            if !is_sha256_hex(&file.sha256) || file.size > MAX_ENTRY_BYTES {
                return Err(malformed("inventory entry has an invalid size or hash"));
            }
            if !keys.insert(collision_key(&file.path)) {
                return Err(malformed("inventory lists colliding paths"));
            }
        }
        Ok(())
    }
}

/// Never removed by an update: Explorer may still have these loaded.
fn is_retained_shell_path(path: &str) -> bool {
    path.starts_with("shell-integration/") || path == "hane_shell_extension.dll"
}

fn backup_path(path: &str) -> String {
    format!("backup/{path}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStep {
    pub rel_path: String,
    /// `None`: the file must not exist before the update.
    pub old: Option<FileIdentity>,
    /// `None`: the file is removed by the update.
    pub new: Option<FileIdentity>,
    pub backup_rel: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyPlan {
    pub install_id: InstallId,
    pub transaction_id: TransactionId,
    pub source: ReleaseIdentity,
    pub destination: ReleaseIdentity,
    pub steps: Vec<PlanStep>,
}

impl ApplyPlan {
    /// Builds the owned-file plan for a Windows update.
    ///
    /// Requires an applicable (strictly newer, signature-verified) candidate
    /// and signature-verified inventories for both sides.
    pub fn for_inventories(
        install_id: InstallId,
        transaction_id: TransactionId,
        old: &SignedInventory,
        new: &SignedInventory,
        candidate: &VerifiedCandidate,
    ) -> Result<Self> {
        if !candidate.is_applicable() {
            return Err(UpdateError::new(
                ErrorCode::NotApplicable,
                "candidate is not a newer verified version",
            ));
        }
        if candidate.target() != Target::WindowsX64 {
            return Err(UpdateError::new(
                ErrorCode::UnsupportedInstall,
                "file-set plans exist only for the Windows layout",
            ));
        }
        new.check_candidate(candidate)?;
        if old.inventory.target != new.inventory.target {
            return Err(UpdateError::new(
                ErrorCode::IdentityMismatch,
                "installed and candidate targets differ",
            ));
        }
        let old_version = crate::version::parse_stable_version(&old.inventory.version)?;
        if relation(&old_version, candidate.version()) != VersionRelation::Newer {
            return Err(UpdateError::new(
                ErrorCode::NotApplicable,
                "installed inventory is not older than the candidate",
            ));
        }

        let old_files = file_map(old);
        let new_files = file_map(new);
        let old_keys: BTreeMap<String, &str> = old_files
            .keys()
            .map(|path| (collision_key(path), path.as_str()))
            .collect();

        let mut steps = Vec::new();
        for (path, new_id) in &new_files {
            if let Some(existing) = old_keys.get(&collision_key(path))
                && *existing != path.as_str()
            {
                return Err(UpdateError::new(
                    ErrorCode::InstallModified,
                    format!("{path} collides with installed {existing} by case"),
                ));
            }
            match old_files.get(path) {
                Some(old_id) if old_id == new_id => {}
                Some(old_id) => steps.push(PlanStep {
                    rel_path: path.clone(),
                    old: Some(old_id.clone()),
                    new: Some(new_id.clone()),
                    backup_rel: Some(backup_path(path)),
                }),
                None => steps.push(PlanStep {
                    rel_path: path.clone(),
                    old: None,
                    new: Some(new_id.clone()),
                    backup_rel: None,
                }),
            }
        }
        for (path, old_id) in &old_files {
            if new_files.contains_key(path) || is_retained_shell_path(path) {
                continue;
            }
            steps.push(PlanStep {
                rel_path: path.clone(),
                old: Some(old_id.clone()),
                new: None,
                backup_rel: Some(backup_path(path)),
            });
        }
        // Managed inventory files are replaced as ordinary owned files.
        for (path, old_id, new_id) in [
            (INVENTORY_FILE_NAME, &old.raw, &new.raw),
            (
                INVENTORY_SIGNATURE_FILE_NAME,
                &old.signature,
                &new.signature,
            ),
        ] {
            steps.push(PlanStep {
                rel_path: path.to_string(),
                old: Some(old_id.clone()),
                new: Some(new_id.clone()),
                backup_rel: Some(backup_path(path)),
            });
        }
        // The running executable is replaced last so an interruption never
        // leaves a window without a launchable `hane.exe`.
        steps.sort_by(|a, b| {
            (a.rel_path == "hane.exe", &a.rel_path).cmp(&(b.rel_path == "hane.exe", &b.rel_path))
        });
        Ok(Self {
            install_id,
            transaction_id,
            source: ReleaseIdentity::from_inventory(old),
            destination: ReleaseIdentity::from_candidate(candidate),
            steps,
        })
    }

    /// Stable hash binding a journal to exactly this plan.
    pub fn plan_hash(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|err| UpdateError::new(ErrorCode::Io, err.to_string()))?;
        Ok(sha256_hex(&bytes))
    }
}

fn file_map(signed: &SignedInventory) -> BTreeMap<String, FileIdentity> {
    signed
        .inventory
        .files
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                FileIdentity {
                    size: file.size,
                    sha256: file.sha256.clone(),
                },
            )
        })
        .collect()
}
