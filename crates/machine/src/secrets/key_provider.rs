//! The `KeyProvider` abstraction: where the key-encryption key lives, and how a data key is wrapped
//! (EXEC-007, DOSSIER.md §"Secrets at rest").
//!
//! Secrets are stored **envelope-encrypted**: each secret gets its own data key, the material is sealed
//! under that data key, and the data key is wrapped by a key-encryption key that never leaves the
//! provider. Rotating a provider's KEK therefore re-wraps data keys rather than re-encrypting secrets,
//! and the provider is the only thing that has to be trusted with the KEK.
//!
//! Two implementations, and the difference between them is where the KEK is:
//!
//! * [`KmsKeyProvider`] delegates `wrap`/`unwrap` to a [`KmsClient`] — a cloud KMS holds the KEK and
//!   performs the operation, so the unwrapped data key exists in this process only for the moment it is
//!   used. This is the managed and private-cloud path.
//! * [`LocalMasterKeyProvider`] holds a master key read from a file with restricted permissions, and
//!   wraps with an AEAD under it. This is the dev and self-host path; it is a real seal, not a stub,
//!   so a test of the shipped path is a test of the shipped cryptography.
//!
//! The frame written to `secret_handles.ciphertext` is versioned so a future change can be recognised
//! rather than guessed at:
//!
//! ```text
//! version (1) ‖ nonce (12) ‖ wrapped_key_len (2, big endian) ‖ wrapped_key ‖ sealed material
//! ```
//!
//! `secret_handles.data_key_ref` names the KEK the data key was wrapped under, which is what lets a
//! rotation find the secrets still sealed under an older key.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use quansio_core::EntropySource;

use super::SecretError;

/// The frame version this module writes.
pub const ENVELOPE_VERSION: u8 = 1;
const NONCE_BYTES: usize = 12;
const DATA_KEY_BYTES: usize = 32;
const LENGTH_BYTES: usize = 2;
pub(super) const HEADER_BYTES: usize = 1 + NONCE_BYTES + LENGTH_BYTES;

/// A key-encryption key, as the broker sees it: it can wrap a data key and unwrap one, and it never
/// hands the KEK itself out.
pub trait KeyProvider: Send + Sync {
    /// The reference recorded on each secret, so a rotation can find what is sealed under this key.
    fn key_ref(&self) -> &str;

    /// Wrap a data key.
    ///
    /// # Errors
    /// Returns [`SecretError::KeyProvider`] when the provider refuses or is unreachable.
    fn wrap(&self, data_key: &[u8]) -> Result<Vec<u8>, SecretError>;

    /// Unwrap a data key.
    ///
    /// # Errors
    /// Returns [`SecretError::KeyProvider`] when the wrapped key is not one this provider sealed — a
    /// wrong key, a tampered frame, or a frame from another provider. Returning the wrong bytes instead
    /// would surface as a decryption failure further away from the cause.
    fn unwrap(&self, wrapped: &[u8]) -> Result<Vec<u8>, SecretError>;
}

/// The operations a cloud KMS must offer. Implementing this is the deployment's job; the broker only
/// needs the two calls, and keeping them behind a trait is what lets the broker be tested against a
/// real seal without pretending to be a cloud.
pub trait KmsClient: Send + Sync {
    /// Encrypt `plaintext` under `key_ref`.
    ///
    /// # Errors
    /// Returns [`SecretError::KeyProvider`] when the KMS refuses.
    fn encrypt(&self, key_ref: &str, plaintext: &[u8]) -> Result<Vec<u8>, SecretError>;

    /// Decrypt `ciphertext` under `key_ref`.
    ///
    /// # Errors
    /// Returns [`SecretError::KeyProvider`] when the KMS refuses.
    fn decrypt(&self, key_ref: &str, ciphertext: &[u8]) -> Result<Vec<u8>, SecretError>;
}

/// A provider whose key-encryption key lives in a cloud KMS.
pub struct KmsKeyProvider<C: KmsClient> {
    client: C,
    key_ref: String,
}

impl<C: KmsClient> KmsKeyProvider<C> {
    /// Bind the provider to one KMS key.
    #[must_use]
    pub fn new(client: C, key_ref: impl Into<String>) -> Self {
        Self {
            client,
            key_ref: key_ref.into(),
        }
    }
}

impl<C: KmsClient> KeyProvider for KmsKeyProvider<C> {
    fn key_ref(&self) -> &str {
        &self.key_ref
    }

    fn wrap(&self, data_key: &[u8]) -> Result<Vec<u8>, SecretError> {
        self.client.encrypt(&self.key_ref, data_key)
    }

    fn unwrap(&self, wrapped: &[u8]) -> Result<Vec<u8>, SecretError> {
        self.client.decrypt(&self.key_ref, wrapped)
    }
}

/// A provider whose master key is read from a file the operator controls (dev and self-host).
pub struct LocalMasterKeyProvider {
    master_key: [u8; DATA_KEY_BYTES],
    key_ref: String,
}

impl std::fmt::Debug for LocalMasterKeyProvider {
    /// Never renders the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalMasterKeyProvider")
            .field("key_ref", &self.key_ref)
            .field("master_key", &"<redacted>")
            .finish()
    }
}

impl LocalMasterKeyProvider {
    /// Build a provider from raw master-key bytes.
    ///
    /// # Errors
    /// Returns [`SecretError::MasterKeyInvalid`] when the bytes are not exactly 32 bytes: a short key is
    /// a misconfiguration to refuse, not one to stretch into shape.
    pub fn from_master_key(bytes: &[u8], key_ref: impl Into<String>) -> Result<Self, SecretError> {
        let master_key: [u8; DATA_KEY_BYTES] =
            bytes
                .try_into()
                .map_err(|_| SecretError::MasterKeyInvalid {
                    detail: format!("expected {DATA_KEY_BYTES} bytes, got {}", bytes.len()),
                })?;
        Ok(Self {
            master_key,
            key_ref: key_ref.into(),
        })
    }

    /// Read the master key from a file, decoding base64.
    ///
    /// # Errors
    /// Returns [`SecretError::MasterKeyInvalid`] when the file cannot be read, is not base64, or does
    /// not decode to 32 bytes.
    pub fn from_file(
        path: &std::path::Path,
        key_ref: impl Into<String>,
    ) -> Result<Self, SecretError> {
        let text =
            std::fs::read_to_string(path).map_err(|error| SecretError::MasterKeyInvalid {
                detail: format!("{}: {error}", path.display()),
            })?;
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .map_err(|error| SecretError::MasterKeyInvalid {
                detail: format!("{} is not base64: {error}", path.display()),
            })?;
        Self::from_master_key(&bytes, key_ref)
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(Key::from_slice(&self.master_key))
    }
}

impl KeyProvider for LocalMasterKeyProvider {
    fn key_ref(&self) -> &str {
        &self.key_ref
    }

    fn wrap(&self, data_key: &[u8]) -> Result<Vec<u8>, SecretError> {
        let cipher = self.cipher();
        // The wrapping nonce is fresh per call, so wrapping the same data key twice produces different
        // bytes and a wrapped key is not a fingerprint of the data key.
        let mut nonce = [0u8; NONCE_BYTES];
        os_entropy().fill(&mut nonce);
        let wrapped = cipher
            .encrypt(Nonce::from_slice(&nonce), data_key)
            .map_err(|_| SecretError::KeyProvider {
                detail: "the master key refused to wrap".to_string(),
            })?;
        let mut out = Vec::with_capacity(NONCE_BYTES + wrapped.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&wrapped);
        Ok(out)
    }

    fn unwrap(&self, wrapped: &[u8]) -> Result<Vec<u8>, SecretError> {
        if wrapped.len() <= NONCE_BYTES {
            return Err(SecretError::KeyProvider {
                detail: "the wrapped data key is truncated".to_string(),
            });
        }
        let (nonce, body) = wrapped.split_at(NONCE_BYTES);
        let cipher = self.cipher();
        cipher
            .decrypt(Nonce::from_slice(nonce), body)
            .map_err(|_| SecretError::KeyProvider {
                detail: "the wrapped data key did not open under this master key".to_string(),
            })
    }
}

/// The entropy the shipped path uses.
fn os_entropy() -> quansio_core::OsEntropy {
    quansio_core::OsEntropy
}

/// A sealed secret: the frame that goes into `secret_handles.ciphertext` and the key it was sealed
/// under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSecret {
    /// The versioned frame.
    pub ciphertext: Vec<u8>,
    /// The provider key reference.
    pub key_ref: String,
}

/// Seal `material` for storage under `provider`.
///
/// The data key is fresh, wrapped by the provider, and never stored unwrapped; the material is sealed
/// with the data key and bound to the wrapped key by the AEAD's associated data, so a frame whose parts
/// were swapped from another secret does not open.
///
/// # Errors
/// Returns [`SecretError::KeyProvider`] when the provider refuses to wrap.
pub fn seal(
    provider: &dyn KeyProvider,
    material: &[u8],
    entropy: &dyn EntropySource,
) -> Result<SealedSecret, SecretError> {
    let mut data_key = [0u8; DATA_KEY_BYTES];
    entropy.fill(&mut data_key);
    let mut nonce = [0u8; NONCE_BYTES];
    entropy.fill(&mut nonce);

    let wrapped_key = provider.wrap(&data_key)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&data_key));
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: material,
                aad: &wrapped_key,
            },
        )
        .map_err(|_| SecretError::KeyProvider {
            detail: "the data key refused to seal".to_string(),
        })?;

    let key_len = u16::try_from(wrapped_key.len()).map_err(|_| SecretError::KeyProvider {
        detail: "the wrapped data key is too long to frame".to_string(),
    })?;
    let mut frame = Vec::with_capacity(HEADER_BYTES + wrapped_key.len() + sealed.len());
    frame.push(ENVELOPE_VERSION);
    frame.extend_from_slice(&nonce);
    frame.extend_from_slice(&key_len.to_be_bytes());
    frame.extend_from_slice(&wrapped_key);
    frame.extend_from_slice(&sealed);
    Ok(SealedSecret {
        ciphertext: frame,
        key_ref: provider.key_ref().to_string(),
    })
}

/// Open a frame produced by [`seal`].
///
/// # Errors
/// Returns [`SecretError::EnvelopeUnsupported`] for a version this module does not know,
/// [`SecretError::EnvelopeMalformed`] for a truncated frame, and [`SecretError::KeyProvider`] when the
/// data key does not open or the material fails its authentication — which is also what a frame from
/// another secret looks like, because of the associated data.
pub fn open(provider: &dyn KeyProvider, frame: &[u8]) -> Result<Vec<u8>, SecretError> {
    if frame.len() < HEADER_BYTES {
        return Err(SecretError::EnvelopeMalformed {
            detail: "shorter than its own header".to_string(),
        });
    }
    let version = frame[0];
    if version != ENVELOPE_VERSION {
        return Err(SecretError::EnvelopeUnsupported { version });
    }
    let nonce = &frame[1..1 + NONCE_BYTES];
    let key_len = usize::from(u16::from_be_bytes([
        frame[1 + NONCE_BYTES],
        frame[2 + NONCE_BYTES],
    ]));
    let body = &frame[HEADER_BYTES..];
    if body.len() < key_len {
        return Err(SecretError::EnvelopeMalformed {
            detail: "the wrapped data key runs past the frame".to_string(),
        });
    }
    let (wrapped_key, sealed) = body.split_at(key_len);
    let data_key = provider.unwrap(wrapped_key)?;
    if data_key.len() != DATA_KEY_BYTES {
        return Err(SecretError::EnvelopeMalformed {
            detail: format!("the data key is {} bytes", data_key.len()),
        });
    }
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&data_key));
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: sealed,
                aad: wrapped_key,
            },
        )
        .map_err(|_| SecretError::SecretMaterialUnreadable)
}
