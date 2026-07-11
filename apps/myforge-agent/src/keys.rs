use std::fs::File;
use std::io::Read;
use std::path::Path;

use ed25519_dalek::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePublicKey};
use ed25519_dalek::{SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::error::AgentError;

const MAX_PEM_BYTES: u64 = 16 * 1024;
const PRIVATE_KEY_HEADER: &str = "-----BEGIN PRIVATE KEY-----";
const PUBLIC_KEY_HEADER: &str = "-----BEGIN PUBLIC KEY-----";

pub struct KeyMaterial {
    agent_signing_key: SigningKey,
    agent_verifying_key: VerifyingKey,
    server_verifying_key: VerifyingKey,
    server_public_key_fingerprint: String,
}

impl std::fmt::Debug for KeyMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyMaterial")
            .field("agent_private_key", &"[REDACTED]")
            .field("agent_public_key", &"configured")
            .field("server_public_key", &"configured")
            .finish()
    }
}

impl KeyMaterial {
    pub fn load(
        agent_private_path: &Path,
        agent_public_path: &Path,
        server_public_path: &Path,
    ) -> Result<Self, AgentError> {
        let private_pem = Zeroizing::new(read_key_file(
            agent_private_path,
            "MYFORGE_AGENT_PRIVATE_KEY_PATH",
        )?);
        let agent_public_pem = read_key_file(agent_public_path, "MYFORGE_AGENT_PUBLIC_KEY_PATH")?;
        let server_public_pem =
            read_key_file(server_public_path, "MYFORGE_SERVER_PUBLIC_KEY_PATH")?;

        if !has_exact_header(&private_pem, PRIVATE_KEY_HEADER) {
            return Err(AgentError::config(
                "MYFORGE_AGENT_PRIVATE_KEY_PATH",
                "expected PKCS#8 Ed25519 private key PEM",
            ));
        }
        if !has_exact_header(&agent_public_pem, PUBLIC_KEY_HEADER) {
            return Err(AgentError::config(
                "MYFORGE_AGENT_PUBLIC_KEY_PATH",
                "expected SPKI Ed25519 public key PEM",
            ));
        }
        if !has_exact_header(&server_public_pem, PUBLIC_KEY_HEADER) {
            return Err(AgentError::config(
                "MYFORGE_SERVER_PUBLIC_KEY_PATH",
                "expected SPKI Ed25519 public key PEM",
            ));
        }

        let agent_signing_key = SigningKey::from_pkcs8_pem(private_pem.as_str()).map_err(|_| {
            AgentError::config(
                "MYFORGE_AGENT_PRIVATE_KEY_PATH",
                "expected PKCS#8 Ed25519 private key PEM",
            )
        })?;
        let agent_verifying_key =
            VerifyingKey::from_public_key_pem(&agent_public_pem).map_err(|_| {
                AgentError::config(
                    "MYFORGE_AGENT_PUBLIC_KEY_PATH",
                    "expected SPKI Ed25519 public key PEM",
                )
            })?;
        let server_verifying_key =
            VerifyingKey::from_public_key_pem(&server_public_pem).map_err(|_| {
                AgentError::config(
                    "MYFORGE_SERVER_PUBLIC_KEY_PATH",
                    "expected SPKI Ed25519 public key PEM",
                )
            })?;
        let server_der = server_verifying_key.to_public_key_der().map_err(|_| {
            AgentError::config(
                "MYFORGE_SERVER_PUBLIC_KEY_PATH",
                "expected SPKI Ed25519 public key PEM",
            )
        })?;
        let server_public_key_fingerprint = format!("{:x}", Sha256::digest(server_der.as_bytes()));

        if agent_signing_key.verifying_key() != agent_verifying_key {
            return Err(AgentError::config(
                "MYFORGE_AGENT_PUBLIC_KEY_PATH",
                "agent private and public keys do not match",
            ));
        }

        Ok(Self {
            agent_signing_key,
            agent_verifying_key,
            server_verifying_key,
            server_public_key_fingerprint,
        })
    }

    pub fn agent_signing_key(&self) -> &SigningKey {
        &self.agent_signing_key
    }

    pub fn agent_verifying_key(&self) -> &VerifyingKey {
        &self.agent_verifying_key
    }

    pub fn server_verifying_key(&self) -> &VerifyingKey {
        &self.server_verifying_key
    }

    pub fn server_public_key_fingerprint(&self) -> &str {
        &self.server_public_key_fingerprint
    }
}

fn read_key_file(path: &Path, variable: &str) -> Result<String, AgentError> {
    let file =
        File::open(path).map_err(|_| AgentError::config(variable, "key file is unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| AgentError::config(variable, "key file is unavailable"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_PEM_BYTES {
        return Err(AgentError::config(
            variable,
            "key file must be a non-empty regular PEM file",
        ));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_PEM_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AgentError::config(variable, "key file is unreadable"))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_PEM_BYTES {
        return Err(AgentError::config(
            variable,
            "key file must be a non-empty regular PEM file",
        ));
    }
    String::from_utf8(bytes).map_err(|_| AgentError::config(variable, "key file must be UTF-8 PEM"))
}

fn has_exact_header(pem: &str, expected: &str) -> bool {
    pem.lines().next() == Some(expected)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
    use pkcs8::LineEnding;
    use tempfile::tempdir;

    use super::*;

    fn write_key_set(seed: u8) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let directory = tempdir().unwrap();
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let private_path = directory.path().join("agent-private.pem");
        let public_path = directory.path().join("agent-public.pem");
        fs::write(
            &private_path,
            signing.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes(),
        )
        .unwrap();
        fs::write(
            &public_path,
            signing
                .verifying_key()
                .to_public_key_pem(LineEnding::LF)
                .unwrap(),
        )
        .unwrap();
        (directory, private_path, public_path)
    }

    #[test]
    fn loads_matching_ed25519_pkcs8_and_spki_keys() {
        let (_directory, private_path, public_path) = write_key_set(7);
        let material = KeyMaterial::load(&private_path, &public_path, &public_path).unwrap();

        assert_eq!(
            material.agent_signing_key().verifying_key(),
            *material.agent_verifying_key()
        );
        assert_eq!(
            material.server_verifying_key(),
            material.agent_verifying_key()
        );
    }

    #[test]
    fn rejects_mismatched_agent_key_pair_without_exposing_paths() {
        let (_first_dir, private_path, _) = write_key_set(8);
        let (_second_dir, _, public_path) = write_key_set(9);

        let error = KeyMaterial::load(&private_path, &public_path, &public_path).unwrap_err();
        let display = error.to_string();
        let debug = format!("{error:?}");

        assert_eq!(error.code(), crate::ErrorCode::ConfigInvalid);
        assert!(display.contains("do not match"));
        assert!(!display.contains(private_path.to_string_lossy().as_ref()));
        assert!(!debug.contains(public_path.to_string_lossy().as_ref()));
    }

    #[test]
    fn rejects_non_pkcs8_private_key_header() {
        let (directory, private_path, public_path) = write_key_set(10);
        fs::write(
            &private_path,
            "-----BEGIN ED25519 PRIVATE KEY-----\nAAAA\n-----END ED25519 PRIVATE KEY-----\n",
        )
        .unwrap();

        let error = KeyMaterial::load(&private_path, &public_path, &public_path).unwrap_err();
        assert!(error.message().contains("PKCS#8 Ed25519"));
        drop(directory);
    }

    #[test]
    fn rejects_non_spki_server_public_key() {
        let (directory, private_path, public_path) = write_key_set(11);
        let server_path = directory.path().join("server.pem");
        fs::write(
            &server_path,
            "-----BEGIN RSA PUBLIC KEY-----\nAAAA\n-----END RSA PUBLIC KEY-----\n",
        )
        .unwrap();

        let error = KeyMaterial::load(&private_path, &public_path, &server_path).unwrap_err();
        assert!(error.message().contains("SPKI Ed25519"));
    }
}
