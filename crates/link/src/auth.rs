//! Per-enable credentials and the helpers that keep secrets out of logs.

use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

/// `n` random bytes as lowercase hex. `None` when the system source fails; the
/// caller then must not start the service.
pub fn random_hex(n: usize) -> Option<String> {
    let mut bytes = vec![0u8; n];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(hex(&bytes))
}

/// SHA-256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}

/// What a running service holds: who it is and the secret a client must send.
/// Regenerated on every start, so disabling and re-enabling the link revokes
/// whatever was handed out before.
#[derive(Clone)]
pub struct Credentials {
    instance_id: String,
    token: String,
}

impl Credentials {
    /// A fresh 128-bit instance id and 256-bit token.
    pub fn generate() -> Option<Self> {
        Some(Self {
            instance_id: random_hex(16)?,
            token: random_hex(32)?,
        })
    }

    pub fn from_parts(instance_id: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            token: token.into(),
        }
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Only the descriptor publisher may read the token.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Constant-time comparison; a length mismatch is rejected first, which
    /// reveals only the (public) token length.
    pub fn verify(&self, presented: &str) -> bool {
        let expected = self.token.as_bytes();
        let presented = presented.as_bytes();
        if expected.len() != presented.len() {
            return false;
        }
        // Fold every byte so the time does not depend on where they differ.
        expected
            .iter()
            .zip(presented)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token never reaches a log through `{:?}`.
        formatter
            .debug_struct("Credentials")
            .field("instance_id", &self.instance_id)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_credentials_have_the_documented_size_and_differ_each_time() {
        let a = Credentials::generate().unwrap();
        let b = Credentials::generate().unwrap();
        assert_eq!(a.instance_id().len(), 32);
        assert_eq!(a.token().len(), 64);
        assert_ne!(a.token(), b.token());
        assert_ne!(a.instance_id(), b.instance_id());
    }

    #[test]
    fn verify_accepts_only_the_exact_token() {
        let credentials = Credentials::from_parts("i", "abcdef");
        assert!(credentials.verify("abcdef"));
        assert!(!credentials.verify("abcdeg"));
        assert!(!credentials.verify("abcde"));
        assert!(!credentials.verify(""));
        assert!(!credentials.verify("abcdef0"));
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let credentials = Credentials::from_parts("inst", "supersecret");
        let shown = format!("{credentials:?}");
        assert!(!shown.contains("supersecret"));
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
