use std::sync::Arc;

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305};

/// Encrypts a single upload chunk. Implementations must be Send + Sync so they
/// can be shared across worker threads (native) and stored in thread-locals (WASM).
///
/// `part_number` is passed so each part gets a unique, deterministic nonce —
/// this makes encryption fully resumable: any part can be re-encrypted given
/// the same key + base_nonce + part_number, with no extra state required.
pub trait ChunkEncryptor: Send + Sync + std::fmt::Debug {
    fn encrypt_chunk(&self, data: &[u8], part_number: u32) -> Vec<u8>;

    /// Extra bytes added per chunk by the AEAD tag (0 for stream ciphers).
    /// The upload engine reads `part_size - ciphertext_overhead()` bytes of
    /// plaintext per chunk so the uploaded ciphertext fits exactly in the
    /// presigned-URL slot.
    fn ciphertext_overhead(&self) -> usize { 0 }

    /// If `Some`, the orchestrator appends a footer as the final S3 part.
    /// The footer encrypts `n_data_parts || effective_part_size` (12 bytes) so
    /// a decoder has everything it needs without external metadata.
    /// Default: `None` (no footer).
    fn compute_footer(&self, _n_data_parts: u32, _effective_part_size: u64) -> Option<Vec<u8>> { None }
}

/// Convenience alias used throughout the codebase.
pub type ArcEncryptor = Arc<dyn ChunkEncryptor>;

/// XChaCha20-Poly1305 chunk encryptor (RustCrypto).
///
/// Security properties:
/// - **Confidentiality**: XChaCha20 stream cipher.
/// - **Integrity / tamper detection**: Poly1305 AEAD tag (16 bytes per chunk).
/// - **Reordering detection**: `part_number` is included as AAD — decrypting
///   a chunk at the wrong position fails tag verification.
/// - **Omission detection**: a footer part is appended that encodes the total
///   data-part count; a missing chunk changes the count and is detected.
///
/// Per-part nonce: `base_nonce[..20] || (base_nonce[20..24] XOR part_number_LE)`
/// This keeps nonces deterministic and collision-free for up to 2^32 parts.
#[derive(Debug, Clone)]
pub struct XChaCha20Poly1305Encryptor {
    key: [u8; 32],
    base_nonce: [u8; 24],
}

impl XChaCha20Poly1305Encryptor {
    /// `key_hex`   — 64 hex chars (32 bytes)
    /// `nonce_hex` — 48 hex chars (24 bytes)
    pub fn from_hex(key_hex: &str, nonce_hex: &str) -> Result<Self, String> {
        let key = hex_to_fixed::<32>(key_hex, "encryption key")?;
        let base_nonce = hex_to_fixed::<24>(nonce_hex, "encryption nonce")?;
        Ok(Self { key, base_nonce })
    }
}

impl ChunkEncryptor for XChaCha20Poly1305Encryptor {
    fn ciphertext_overhead(&self) -> usize { 16 }

    fn encrypt_chunk(&self, data: &[u8], part_number: u32) -> Vec<u8> {
        let mut nonce = self.base_nonce;
        let n = part_number.to_le_bytes();
        nonce[20] ^= n[0]; nonce[21] ^= n[1]; nonce[22] ^= n[2]; nonce[23] ^= n[3];

        let cipher = XChaCha20Poly1305::new((&self.key).into());
        cipher
            .encrypt(
                (&nonce).into(),
                Payload { msg: data, aad: &part_number.to_le_bytes() },
            )
            .expect("xchacha20poly1305 encrypt")
    }

    fn compute_footer(&self, n_data_parts: u32, effective_part_size: u64) -> Option<Vec<u8>> {
        // Footer plaintext: n_data_parts (4 bytes LE) || effective_part_size (8 bytes LE)
        // This makes the encrypted file self-describing — a decoder only needs key + nonce.
        let mut plaintext = [0u8; 12];
        plaintext[..4].copy_from_slice(&n_data_parts.to_le_bytes());
        plaintext[4..].copy_from_slice(&effective_part_size.to_le_bytes());
        Some(self.encrypt_chunk(&plaintext, n_data_parts + 1))
    }
}

fn hex_to_fixed<const N: usize>(hex: &str, label: &str) -> Result<[u8; N], String> {
    if hex.len() != N * 2 {
        return Err(format!(
            "Invalid {label} length: expected {} hex chars, got {}",
            N * 2,
            hex.len()
        ));
    }
    let mut bytes = [0u8; N];
    for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
        bytes[i] = u8::from_str_radix(s, 16)
            .map_err(|e| format!("Invalid hex in {label}: {e}"))?;
    }
    Ok(bytes)
}
