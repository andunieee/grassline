//! The few pieces of the hyperb protocol the admin interface needs (see the
//! `SPEC.md` of hyperb): the HTTP header names, the iroh ALPN and the keys.

use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::str::FromStr;

pub const KEY: &str = "hyperb-key";
pub const SESSION: &str = "hyperb-session";
pub const SIGNATURE: &str = "hyperb-signature";
pub const IROH_ALPN: &str = "hyperb/0";

/// Splits one line from a server into the SSE event name and data.
pub fn line_to_event(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_end_matches(['\n', '\r']);
    line.split_once(' ').filter(|(tag, _)| !tag.is_empty())
}

/// The path an ALPN of an iroh session stands for, or `None` if it is not a
/// hyperb ALPN.
pub fn iroh_alpn_path(alpn: &[u8]) -> Option<String> {
    let rest = std::str::from_utf8(alpn).ok()?.strip_prefix(IROH_ALPN)?;
    match rest.chars().next() {
        None => Some("/".into()),
        Some('/') => Some(rest.into()),
        Some('?') => Some(format!("/{rest}")),
        Some(_) => None,
    }
}

/// An ed25519 keypair, the kind iroh uses for endpoint ids.
#[derive(Clone)]
pub struct Identity(SigningKey);

impl Identity {
    pub fn secret(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// Loads the identity kept at `path` (64 hex digits), creating it if there
    /// is none. A file that exists but cannot be read is an error, never
    /// replaced.
    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let mut secret = [0u8; 32];
                hex::decode_to_slice(s.trim(), &mut secret).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{}: {e}", path.display()),
                    )
                })?;
                Ok(Identity(SigningKey::from_bytes(&secret)))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let mut secret = [0u8; 32];
                getrandom::getrandom(&mut secret).expect("system random number generator");
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
                writeln!(opts.open(path)?, "{}", hex::encode(secret))?;
                Ok(Identity(SigningKey::from_bytes(&secret)))
            }
            Err(e) => Err(e),
        }
    }
}

/// A renderer's public key; displayed and parsed as hex.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    /// Whether `signature` (hex) is this key's signature of `msg`.
    pub fn verify(&self, msg: &[u8], signature: &str) -> bool {
        let mut sig = [0u8; 64];
        hex::decode_to_slice(signature.trim(), &mut sig).is_ok()
            && self.0.verify(msg, &Signature::from_bytes(&sig)).is_ok()
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0.as_bytes()))
    }
}

impl FromStr for PublicKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(s.trim(), &mut bytes).map_err(|e| e.to_string())?;
        VerifyingKey::from_bytes(&bytes)
            .map(PublicKey)
            .map_err(|_| "not an ed25519 public key".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;

    #[test]
    fn verifies_signatures() {
        let id = Identity::load_or_create(
            &std::env::temp_dir().join(format!("grassline-id-{}", std::process::id())),
        )
        .unwrap();
        let key: PublicKey = hex::encode(id.0.verifying_key().as_bytes())
            .parse()
            .unwrap();
        let sig = hex::encode(id.0.sign(b"s").to_bytes());
        assert!(key.verify(b"s", &sig));
        assert!(!key.verify(b"t", &sig));
        assert!(!key.verify(b"s", "zz"));
    }

    #[test]
    fn alpn_paths() {
        assert_eq!(iroh_alpn_path(b"hyperb/0"), Some("/".into()));
        assert_eq!(iroh_alpn_path(b"hyperb/0/admin"), Some("/admin".into()));
        assert_eq!(iroh_alpn_path(b"grassline/0"), None);
    }
}
