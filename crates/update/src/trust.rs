//! Signature verification over raw bytes plus the persisted sequence floor.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, Result, UpdateError};
use crate::limits::MAX_SIGNATURE_BYTES;
use crate::model::{SIGNATURE_SCHEMA, SignatureFile};
use crate::util::{is_sha256_hex, parse_strict, parse_strict_value};

const MAX_SIGNATURES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedKey {
    pub key_id: String,
    pub public_key: [u8; 32],
}

/// Embedded trust material: Ed25519 public keys and the pinned macOS Team ID.
///
/// [`TrustPolicy::production`] contains only what the maintainer supplied to the
/// release build through `HANE_UPDATE_KEY_ID`, `HANE_UPDATE_PUBLIC_KEY_BASE64`
/// (raw 32-byte Ed25519 key) and `HANE_MACOS_TEAM_ID`. A build without them, or
/// with a malformed value, embeds nothing: every verification then fails with
/// `trust_unconfigured`, so no candidate can be made applicable. The unknown key
/// IDs in a fetched signature file are never auto-registered.
#[derive(Debug, Clone, Default)]
pub struct TrustPolicy {
    keys: Vec<TrustedKey>,
    macos_team_id: Option<String>,
}

impl TrustPolicy {
    pub fn production() -> Self {
        Self::from_embedded(
            option_env!("HANE_UPDATE_KEY_ID"),
            option_env!("HANE_UPDATE_PUBLIC_KEY_BASE64"),
            option_env!("HANE_MACOS_TEAM_ID"),
        )
    }

    /// Fail-closed construction from build-time values: anything missing or
    /// malformed yields no key (or no Team ID) instead of a guessed default.
    pub fn from_embedded(
        key_id: Option<&str>,
        public_key_base64: Option<&str>,
        macos_team_id: Option<&str>,
    ) -> Self {
        let key = match (key_id, public_key_base64) {
            (Some(id), Some(encoded))
                if !id.is_empty()
                    && id.len() <= 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.') =>
            {
                STANDARD
                    .decode(encoded.trim())
                    .ok()
                    .and_then(|raw| <[u8; 32]>::try_from(raw.as_slice()).ok())
                    .map(|public_key| TrustedKey {
                        key_id: id.to_string(),
                        public_key,
                    })
            }
            _ => None,
        };
        let team = macos_team_id
            .filter(|t| t.len() == 10 && t.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
            .map(str::to_string);
        Self {
            keys: key.into_iter().collect(),
            macos_team_id: team,
        }
    }

    pub fn new(keys: Vec<TrustedKey>, macos_team_id: Option<String>) -> Self {
        Self {
            keys,
            macos_team_id,
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.keys.is_empty()
    }

    pub fn macos_team_id(&self) -> Option<&str> {
        self.macos_team_id.as_deref()
    }

    fn key(&self, key_id: &str) -> Option<&TrustedKey> {
        self.keys.iter().find(|k| k.key_id == key_id)
    }
}

/// Verifies that a trusted key signed `domain || payload`. Returns the key ID.
///
/// The payload is the exact bytes that were fetched; it is never re-serialized.
pub fn verify_signature(
    policy: &TrustPolicy,
    domain: &[u8],
    payload: &[u8],
    signature_file: &[u8],
) -> Result<String> {
    if !policy.is_configured() {
        return Err(UpdateError::new(
            ErrorCode::TrustUnconfigured,
            "no trusted update key is embedded in this build",
        ));
    }
    if signature_file.len() as u64 > MAX_SIGNATURE_BYTES {
        return Err(UpdateError::new(
            ErrorCode::LimitExceeded,
            "signature file exceeds the size limit",
        ));
    }
    let probe = parse_strict_value(signature_file)?;
    if probe.get("schema").and_then(serde_json::Value::as_u64) != Some(SIGNATURE_SCHEMA) {
        return Err(UpdateError::new(
            ErrorCode::SchemaUnsupported,
            "unknown signature file schema",
        ));
    }
    let file: SignatureFile = parse_strict(signature_file)?;
    if file.signatures.is_empty() || file.signatures.len() > MAX_SIGNATURES {
        return Err(UpdateError::new(
            ErrorCode::SignatureInvalid,
            "signature count out of range",
        ));
    }
    let mut message = Vec::with_capacity(domain.len() + payload.len());
    message.extend_from_slice(domain);
    message.extend_from_slice(payload);

    let mut saw_known_key = false;
    for entry in &file.signatures {
        let Some(key) = policy.key(&entry.key_id) else {
            continue;
        };
        saw_known_key = true;
        let Ok(signature) = STANDARD.decode(&entry.signature_base64) else {
            continue;
        };
        if signature.len() != 64 {
            continue;
        }
        if UnparsedPublicKey::new(&ED25519, &key.public_key)
            .verify(&message, &signature)
            .is_ok()
        {
            return Ok(key.key_id.clone());
        }
    }
    let detail = if saw_known_key {
        "no valid signature from a trusted key"
    } else {
        "signature key is not trusted"
    };
    Err(UpdateError::new(ErrorCode::SignatureInvalid, detail))
}

#[cfg(test)]
mod embedded_tests {
    use super::*;

    #[test]
    fn embedded_values_must_be_complete_and_well_formed() {
        let key = STANDARD.encode([7u8; 32]);
        let ok = TrustPolicy::from_embedded(Some("hane-2026"), Some(&key), Some("ABCDE12345"));
        assert!(ok.is_configured());
        assert_eq!(ok.macos_team_id(), Some("ABCDE12345"));

        for (id, key_b64, team) in [
            (None, Some(key.as_str()), None),
            (Some("hane-2026"), None, None),
            (Some(""), Some(key.as_str()), None),
            (Some("bad id"), Some(key.as_str()), None),
            (Some("hane-2026"), Some("not base64!"), None),
        ] {
            assert!(!TrustPolicy::from_embedded(id, key_b64, team).is_configured());
        }
        let short = STANDARD.encode([7u8; 31]);
        assert!(!TrustPolicy::from_embedded(Some("k"), Some(&short), None).is_configured());
        let bad_team = TrustPolicy::from_embedded(Some("k"), Some(&key), Some("abc"));
        assert_eq!(bad_team.macos_team_id(), None);
    }
}

/// Highest catalog sequence already verified for this installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeenCatalog {
    pub schema: u64,
    pub sequence: u64,
    pub catalog_sha256: String,
}

impl SeenCatalog {
    pub fn new(sequence: u64, catalog_sha256: String) -> Self {
        Self {
            schema: 1,
            sequence,
            catalog_sha256,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 || !is_sha256_hex(&self.catalog_sha256) {
            return Err(UpdateError::new(
                ErrorCode::ReceiptUnwritable,
                "stored catalog receipt is invalid",
            ));
        }
        Ok(())
    }
}

/// Persistence for the per-installation sequence floor. Implementations must
/// fail loudly: a candidate is never made Ready if this cannot be stored.
pub trait SeenStore {
    /// Serializes load / compare / store of the sequence floor across every
    /// process of this placement. The returned guard releases it on drop. A
    /// store shared by several processes must override this; the default suits
    /// a store only one owner can reach.
    fn lock(&self) -> Result<Box<dyn Send>> {
        Ok(Box::new(()))
    }
    fn load(&self) -> Result<Option<SeenCatalog>>;
    fn store(&mut self, seen: &SeenCatalog) -> Result<()>;
}
