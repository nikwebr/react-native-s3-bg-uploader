use std::ffi::c_void;
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
    fn ciphertext_overhead(&self) -> usize {
        0
    }

    /// If `Some`, the footer is appended to the final data part.
    /// The footer encrypts `n_data_parts || effective_part_size` (12 bytes) so
    /// a decoder has everything it needs without external metadata.
    /// Default: `None` (no footer).
    fn compute_footer(&self, _n_data_parts: u32, _effective_part_size: u64) -> Option<Vec<u8>> {
        None
    }

    /// Whether the wire format ends with a synthesised descriptor.
    ///
    /// The descriptor is appended to the **last data part** — never to a part of its
    /// own, because S3 requires every part except the last to be >= 5 MiB and a tiny
    /// trailing part would demote the real last part below that minimum. This flag
    /// only decides whether the descriptor is there at all.
    /// Defaults to `ciphertext_overhead() > 0` — exactly the built-in AEAD layout. A
    /// custom scheme that carries its metadata per chunk overrides this to `false`.
    fn has_footer(&self) -> bool {
        self.ciphertext_overhead() > 0
    }

    /// Bytes the descriptor occupies in the last data part; `0` when there is none.
    /// Defaults to the documented descriptor shape — 12 bytes of plaintext
    /// (`n_data_parts` ‖ `effective_part_size`) protected by the scheme's own tag.
    fn footer_len(&self) -> usize {
        if self.has_footer() {
            12 + self.ciphertext_overhead()
        } else {
            0
        }
    }
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
/// - **Omission detection**: a descriptor is appended to the last part that encodes
///   the total data-part count; a missing chunk changes the count and is detected.
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
    fn ciphertext_overhead(&self) -> usize {
        16
    }

    fn encrypt_chunk(&self, data: &[u8], part_number: u32) -> Vec<u8> {
        let mut nonce = self.base_nonce;
        let n = part_number.to_le_bytes();
        nonce[20] ^= n[0];
        nonce[21] ^= n[1];
        nonce[22] ^= n[2];
        nonce[23] ^= n[3];

        let cipher = XChaCha20Poly1305::new((&self.key).into());
        cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: data,
                    aad: &part_number.to_le_bytes(),
                },
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
        bytes[i] = u8::from_str_radix(s, 16).map_err(|e| format!("Invalid hex in {label}: {e}"))?;
    }
    Ok(bytes)
}

// ===========================================================================
//  Custom (host-provided) encryptor — stable FFI interface
// ===========================================================================

/// Result returned by a host callback (`encrypt_chunk`, `compute_footer`).
///
/// The host owns the allocation. The uploader copies the bytes it needs and then
/// calls the `free` callback, so the host may allocate with any allocator (Rust,
/// C `malloc`, Swift, …) as long as `free` releases it.
#[repr(C)]
pub struct EncryptResult {
    /// Pointer to the output bytes (owned by the host).
    pub data: *mut u8,
    /// Number of valid bytes in `data`.
    pub len: usize,
    /// Total capacity of the allocation (passed back to `free`).
    pub capacity: usize,
}

/// Builds the descriptor (footer) part.
/// `part_number` for this part is always `n_data_parts + 1`.
pub type EncryptFooterFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    n_data_parts: u32,
    effective_part_size: u64,
) -> EncryptResult;

/// Placeholder for [`EncryptorVTable::compute_footer`] when
/// [`EncryptorVTable::has_footer`] is `false` — it is never called. Rust hosts
/// must supply *some* function (function pointers are non-nullable); C/C++/Swift
/// hosts may pass `NULL` instead.
pub unsafe extern "C" fn no_descriptor(
    _ctx: *mut c_void,
    _n_data_parts: u32,
    _effective_part_size: u64,
) -> EncryptResult {
    EncryptResult {
        data: std::ptr::null_mut(),
        len: 0,
        capacity: 0,
    }
}

/// Virtual table a host provides to plug in its own chunk-encryption scheme.
///
/// This is the whole interface — every field is read unconditionally. A scheme
/// must work **part by part**: the engine hands over one S3 part at a time and
/// never the whole file, so schemes needing a single pass over the file cannot
/// be expressed (see the README section "What the ABI cannot express").
#[repr(C)]
pub struct EncryptorVTable {
    /// Create an encryptor instance bound to one upload's key material.
    ///
    /// Called once per upload with the key/nonce passed to
    /// `encryptAndUploadFile`, so two uploads in flight at the same time may use
    /// different keys. Return a host-owned context, or null to fall back to the
    /// built-in cipher for that upload (e.g. when the scheme uses a device key
    /// that ignores the per-upload key).
    ///
    /// `destroy` is called exactly once per context when the upload's session
    /// entry is dropped (finished, cancelled or evicted).
    pub create: unsafe extern "C" fn(
        key: *const u8,
        key_len: usize,
        nonce: *const u8,
        nonce_len: usize,
    ) -> *mut c_void,
    /// Encrypt a single chunk (one S3 part). Returns an `EncryptResult`.
    /// The host must allocate the output on the heap.
    /// `part_number` is the 1-based S3 part index — derive a deterministic
    /// per-part nonce from it so a resumed upload re-encrypts a part identically.
    pub encrypt_chunk: unsafe extern "C" fn(
        ctx: *mut c_void,
        input: *const u8,
        input_len: usize,
        part_number: u32,
    ) -> EncryptResult,
    /// Release a result previously returned by `encrypt_chunk`/`compute_footer`.
    pub free: unsafe extern "C" fn(ctx: *mut c_void, data: *mut u8, len: usize, capacity: usize),
    /// Destroy the context (release resources). Called when the encryptor is dropped.
    pub destroy: unsafe extern "C" fn(ctx: *mut c_void),
    /// Extra ciphertext bytes this scheme adds to every chunk (AEAD tag, IV
    /// prefix, …). The engine reads `part_size - ciphertext_overhead` plaintext
    /// bytes per part so the ciphertext fits the presigned-URL slot exactly.
    /// Use `0` for a scheme that adds nothing per chunk.
    pub ciphertext_overhead: u32,
    /// Whether the wire format ends with a synthesised descriptor.
    /// `false` = metadata is self-contained per chunk and `compute_footer` is never
    /// called.
    pub has_footer: bool,
    /// Bytes [`Self::compute_footer`] returns. The engine reserves this many bytes in
    /// every part so the ciphertext of the final part (data ‖ descriptor) still fits
    /// the presigned-URL slot. Must be non-zero when `has_footer` is `true`.
    pub footer_len: u32,
    /// Descriptor builder — called only when `has_footer` is `true`.
    pub compute_footer: EncryptFooterFn,
}

/// A host-provided encryptor that delegates to a vtable.
pub struct CustomEncryptor {
    /// Opaque context supplied by the host at registration.
    ctx: *mut c_void,
    /// Vtable supplied by the host — must outlive this struct.
    vtable: &'static EncryptorVTable,
}

// Safety: CustomEncryptor only contains raw pointers to host-managed memory and
// plain function pointers. The host guarantees the vtable and context outlive it.
unsafe impl Send for CustomEncryptor {}
unsafe impl Sync for CustomEncryptor {}

impl std::fmt::Debug for CustomEncryptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomEncryptor")
            .field("overhead", &self.vtable.ciphertext_overhead)
            .field("has_footer", &self.vtable.has_footer)
            .finish()
    }
}

impl CustomEncryptor {
    /// Create a new custom encryptor from a vtable.
    ///
    /// # Safety
    /// - `vtable` must point to a valid `EncryptorVTable` that remains valid
    ///   for the lifetime of this `CustomEncryptor`.
    /// - `ctx` must be a valid pointer owned by the host.
    pub unsafe fn from_vtable(ctx: *mut c_void, vtable: &'static EncryptorVTable) -> Self {
        Self { ctx, vtable }
    }

    /// Take ownership of a host callback result: copy it out and release the
    /// host allocation, so the ABI does not depend on a shared allocator.
    unsafe fn take_result(&self, result: EncryptResult) -> Vec<u8> {
        let out = if result.data.is_null() || result.len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(result.data, result.len) }.to_vec()
        };
        if !result.data.is_null() {
            unsafe { (self.vtable.free)(self.ctx, result.data, result.len, result.capacity) };
        }
        out
    }
}

impl ChunkEncryptor for CustomEncryptor {
    fn encrypt_chunk(&self, data: &[u8], part_number: u32) -> Vec<u8> {
        let result = unsafe {
            (self.vtable.encrypt_chunk)(self.ctx, data.as_ptr(), data.len(), part_number)
        };
        unsafe { self.take_result(result) }
    }

    fn ciphertext_overhead(&self) -> usize {
        self.vtable.ciphertext_overhead as usize
    }

    fn has_footer(&self) -> bool {
        self.vtable.has_footer
    }

    fn footer_len(&self) -> usize {
        if self.vtable.has_footer {
            self.vtable.footer_len as usize
        } else {
            0
        }
    }

    fn compute_footer(&self, n_data_parts: u32, effective_part_size: u64) -> Option<Vec<u8>> {
        if !self.vtable.has_footer {
            return None;
        }
        let result = unsafe {
            (self.vtable.compute_footer)(self.ctx, n_data_parts, effective_part_size)
        };
        Some(unsafe { self.take_result(result) })
    }
}

impl Drop for CustomEncryptor {
    fn drop(&mut self) {
        // Call the host's destroy callback to release resources.
        unsafe {
            (self.vtable.destroy)(self.ctx);
        }
    }
}

/// Global state: the registered encryptor factory (if any).
///
/// Only the vtable is stored — key material lives in the per-upload context that
/// `create` builds, so concurrent uploads may use different keys.
static CUSTOM_VTABLE: std::sync::OnceLock<&'static EncryptorVTable> = std::sync::OnceLock::new();

/// Register a host-provided encryptor factory — the entry point host apps call.
///
/// Call this once at startup, before the first upload; later calls are no-ops.
/// No key material is passed here: it comes from each `encryptAndUploadFile` call
/// and is handed to [`EncryptorVTable::create`], so per-upload keys work with a
/// custom scheme exactly as they do with the built-in one.
///
/// # Safety
/// `vtable` must point to a valid [`EncryptorVTable`] that remains valid for the
/// lifetime of the application.
pub extern "C" fn register_custom_encryptor(vtable: *const EncryptorVTable) {
    if vtable.is_null() {
        return;
    }
    // Safety: the caller guarantees the vtable is valid and outlives the
    // application. It is read-only after registration.
    let vtable_ref = unsafe { &*vtable };

    // Fail fast instead of calling through a null pointer during an upload later.
    if (vtable_ref.create as usize) == 0 {
        log::error!("register_custom_encryptor: refusing to register — create is null");
        return;
    }
    if vtable_ref.has_footer
        && ((vtable_ref.compute_footer as usize) == 0 || vtable_ref.footer_len == 0)
    {
        log::error!(
            "register_custom_encryptor: refusing to register — has_footer is true but \
             compute_footer is null or footer_len is 0"
        );
        return;
    }

    match CUSTOM_VTABLE.set(vtable_ref) {
        Ok(()) => log::info!(
            "register_custom_encryptor: factory registered (overhead={}, has_footer={}, footer_len={})",
            vtable_ref.ciphertext_overhead, vtable_ref.has_footer, vtable_ref.footer_len
        ),
        Err(_) => log::warn!("register_custom_encryptor: already registered; ignoring"),
    }
}

/// Whether a host encryptor factory is registered.
pub fn custom_factory_registered() -> bool {
    CUSTOM_VTABLE.get().is_some()
}

/// Build the encryptor for one upload from its key material.
///
/// A registered factory is asked first; if it declines (returns null) or none is
/// registered, the built-in XChaCha20-Poly1305 cipher is used. The result is
/// stored in that file's session entry, so a resume re-uses the same instance
/// only if the key material is identical.
pub fn build_encryptor(key_hex: &str, nonce_hex: &str) -> Result<ArcEncryptor, String> {
    log::debug!(
        "build_encryptor: factory_registered={} key_hex_len={} nonce_hex_len={}",
        CUSTOM_VTABLE.get().is_some(),
        key_hex.len(),
        nonce_hex.len()
    );
    if let Some(vtable) = CUSTOM_VTABLE.get() {
        let key = hex_decode(key_hex, "encryption key")?;
        let nonce = hex_decode(nonce_hex, "encryption nonce")?;
        log::debug!(
            "build_encryptor: calling create(key_len={}, nonce_len={})",
            key.len(),
            nonce.len()
        );
        let ctx = unsafe { (vtable.create)(key.as_ptr(), key.len(), nonce.as_ptr(), nonce.len()) };
        if !ctx.is_null() {
            log::info!("build_encryptor: custom instance created");
            return Ok(Arc::new(unsafe {
                CustomEncryptor::from_vtable(ctx, vtable)
            }));
        }
        log::info!(
            "build_encryptor: custom create() returned null -> built-in cipher for this upload"
        );
    }
    Ok(Arc::new(XChaCha20Poly1305Encryptor::from_hex(
        key_hex, nonce_hex,
    )?))
}

/// Decode hex into raw bytes — key material handed to a host factory.
pub(crate) fn hex_decode(hex: &str, label: &str) -> Result<Vec<u8>, String> {
    if !hex.is_ascii() {
        return Err(format!("Invalid {label}: expected ASCII hex characters"));
    }
    if hex.len() % 2 != 0 {
        return Err(format!(
            "Invalid {label}: expected an even number of hex characters, got {}",
            hex.len()
        ));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| format!("Invalid {label}: non-hex character at byte {}", i / 2))
        })
        .collect()
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    /// The vtable is the ABI boundary, and hosts mirror this struct in their own
    /// crate (they deliberately do not depend on this one), so its layout is
    /// pinned here. If a field is added or reordered, update the mirrors — and the
    /// host crate's `test_vtable_layout_matches_uploader` — together with these
    /// numbers. A mismatch compiles fine and crashes at the first encrypted part.
    #[test]
    fn vtable_layout_is_pinned() {
        use std::mem::{align_of, offset_of, size_of};
        assert_eq!(size_of::<EncryptorVTable>(), 56);
        assert_eq!(align_of::<EncryptorVTable>(), 8);
        assert_eq!(offset_of!(EncryptorVTable, create), 0);
        assert_eq!(offset_of!(EncryptorVTable, encrypt_chunk), 8);
        assert_eq!(offset_of!(EncryptorVTable, free), 16);
        assert_eq!(offset_of!(EncryptorVTable, destroy), 24);
        assert_eq!(offset_of!(EncryptorVTable, ciphertext_overhead), 32);
        assert_eq!(offset_of!(EncryptorVTable, has_footer), 36);
        assert_eq!(offset_of!(EncryptorVTable, footer_len), 40);
        assert_eq!(offset_of!(EncryptorVTable, compute_footer), 48);
    }

    /// The built-in scheme's declared descriptor size must equal what it really
    /// produces, or the engine reserves the wrong number of bytes per part.
    #[test]
    fn builtin_footer_len_matches_descriptor() {
        let enc = XChaCha20Poly1305Encryptor::from_hex(&"ab".repeat(32), &"cd".repeat(24)).unwrap();
        assert_eq!(enc.footer_len(), 28);
        assert_eq!(enc.compute_footer(3, 1024).unwrap().len(), 28);
    }
}
