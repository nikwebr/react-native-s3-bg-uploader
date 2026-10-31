//! Custom encryption library for react-native-s3-bg-uploader.
//!
//! Provides an AES-256-GCM `ChunkEncryptor` that is registered with the uploader
//! at runtime through the stable `EncryptorVTable` C ABI.
//!
//! # Linking model
//!
//! This crate deliberately does **NOT** depend on (and therefore does not link) the
//! `uploader` crate. It mirrors the small `#[repr(C)]` FFI structs locally and
//! declares the uploader's `register_custom_encryptor` symbol as an `extern "C"`
//! import, so it is resolved at link time from the `libuploader.framework` that the
//! app already links through the S3BgUploader pod. That way there is exactly ONE
//! copy of the uploader in the process — the dynamic one — and the custom encryptor
//! is a thin adapter that only owns the AES-GCM code.
//!
//! # Usage
//!
//! 1. Link `libcustom_encryptor.a` and `libuploader.framework` into the app.
//! 2. Call `CustomEncryptor.register()` at startup (Swift), which
//!    calls `custom_encryptor_register()` below.
//! 3. Use `S3BgUploader.encryptAndUploadFile()`.

use std::ffi::c_void;

// ---------------------------------------------------------------------------
// C ABI — must stay layout-identical to uploader/src/core/encryption.rs
// (`#[repr(C)]`, same field order/types; also mirrored in ios/rust/uploader.h).
// ---------------------------------------------------------------------------

/// Result returned by the host's `encrypt_chunk` callback.
#[repr(C)]
pub struct EncryptResult {
    /// Pointer to the ciphertext (owned by the host).
    pub data: *mut u8,
    /// Number of bytes of valid ciphertext in `data`.
    pub len: usize,
    /// Total capacity of the allocation (for `free`).
    pub capacity: usize,
}

/// Builds the descriptor (footer) part — the final S3 part.
pub type EncryptFooterFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    n_data_parts: u32,
    effective_part_size: u64,
) -> EncryptResult;

#[cfg(not(feature = "wasm"))]
/// Virtual table the host provides to implement encryption.
///
/// Layout must stay identical to `uploader/src/core/encryption.rs` (the whole
/// interface, read unconditionally) — a mismatch is silent at compile time and
/// crashes at the first encrypted part, so keep the field order in sync.
#[repr(C)]
pub struct EncryptorVTable {
    /// Create the per-upload context from this upload's key material.
    /// Return null to have the uploader fall back to its built-in cipher.
    pub create: unsafe extern "C" fn(
        key: *const u8,
        key_len: usize,
        nonce: *const u8,
        nonce_len: usize,
    ) -> *mut c_void,
    /// Encrypt a single chunk. `part_number` is the 1-based S3 part index.
    pub encrypt_chunk: unsafe extern "C" fn(
        ctx: *mut c_void,
        input: *const u8,
        input_len: usize,
        part_number: u32,
    ) -> EncryptResult,
    /// Free a result previously returned by `encrypt_chunk`/`compute_footer`.
    pub free: unsafe extern "C" fn(ctx: *mut c_void, data: *mut u8, len: usize, capacity: usize),
    /// Destroy the context (release resources). Called when the encryptor is dropped.
    pub destroy: unsafe extern "C" fn(ctx: *mut c_void),
    /// Extra ciphertext bytes added per chunk (AES-GCM: the 16-byte tag).
    pub ciphertext_overhead: u32,
    /// Whether the wire format ends with a descriptor (true here).
    pub has_footer: bool,
    /// Bytes `custom_compute_footer` returns. The engine reserves them in every part
    /// so the final part (ciphertext ‖ descriptor) still fits the presigned slot.
    pub footer_len: u32,
    /// Builds the descriptor — see `custom_compute_footer`.
    pub compute_footer: EncryptFooterFn,
}

#[cfg(not(feature = "wasm"))]
/// Virtual table handed to the uploader once at startup.
///
/// Key material is deliberately not part of the table: the uploader calls
/// [`create_ctx`] once per upload with that upload's key/nonce, so two files in
/// flight at the same time can use different keys.
static VTABLE: EncryptorVTable = EncryptorVTable {
    create: create_ctx,
    encrypt_chunk: custom_encrypt_chunk,
    free: custom_free,
    destroy: custom_destroy,
    ciphertext_overhead: AES_GCM_TAG_LEN as u32,
    has_footer: true,
    footer_len: DESCRIPTOR_LEN as u32,
    compute_footer: custom_compute_footer,
};

#[cfg(not(feature = "wasm"))]
/// Build the per-upload context from the key material of one upload.
///
/// The uploader passes the bytes behind the hex key/nonce of
/// `encryptAndUploadFile` (32-byte key, 24-byte nonce). Returning null makes the
/// uploader fall back to its built-in cipher for that upload instead of failing.
unsafe extern "C" fn create_ctx(
    key: *const u8,
    key_len: usize,
    nonce: *const u8,
    nonce_len: usize,
) -> *mut c_void {
    if key.is_null() || nonce.is_null() || key_len != 32 || nonce_len != 24 {
        eprintln!(
            "create_ctx: unsupported key material (key_len={}, nonce_len={})",
            key_len, nonce_len
        );
        return std::ptr::null_mut();
    }
    let mut key_arr = [0u8; 32];
    key_arr.copy_from_slice(unsafe { std::slice::from_raw_parts(key, 32) });
    let mut nonce_arr = [0u8; 24];
    nonce_arr.copy_from_slice(unsafe { std::slice::from_raw_parts(nonce, 24) });

    let encryptor = Aes256GcmEncryptor {
        key: key_arr,
        base_nonce: nonce_arr,
    };
    Box::into_raw(Box::new(CustomEncryptorContext { encryptor })) as *mut c_void
}

#[cfg(not(feature = "wasm"))]
/// JNI: pointer to the static vtable — the Android registration entry point.
#[no_mangle]
pub unsafe extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_00024Companion_nativeGetVtablePointer(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) -> u64 {
    (&VTABLE as *const EncryptorVTable) as u64
}

// ---------------------------------------------------------------------------
// Custom AES-256-GCM encryptor
// ---------------------------------------------------------------------------

/// AES-256-GCM chunk encryptor.
///
/// Security properties:
/// - **Confidentiality**: AES-256 in GCM mode.
/// - **Integrity**: GCM provides a 128-bit authentication tag.
/// - **Deterministic**: same key + nonce + data always produce the same ciphertext,
///   so a paused upload can be resumed and re-encrypt any part identically.
/// AES-GCM authentication tag length — the per-chunk ciphertext overhead this
/// scheme declares to the uploader (the vtable's `ciphertext_overhead`).
const AES_GCM_TAG_LEN: usize = 16;

/// Descriptor length: 12 bytes of plaintext (`n_data_parts` ‖ `effective_part_size`)
/// plus the GCM tag. Declared to the uploader as `footer_len`.
const DESCRIPTOR_LEN: usize = 12 + AES_GCM_TAG_LEN;

#[derive(Debug, Clone)]
pub struct Aes256GcmEncryptor {
    key: [u8; 32],
    base_nonce: [u8; 24], // 24-byte base nonce (48 hex chars) as required by the uploader
}

impl Aes256GcmEncryptor {
    /// Create from hex-encoded key (64 chars) and nonce (48 chars).
    pub fn from_hex(key_hex: &str, nonce_hex: &str) -> Result<Self, String> {
        let key = hex_to_fixed::<32>(key_hex, "AES key")?;
        let base_nonce = hex_to_fixed::<24>(nonce_hex, "AES nonce")?;
        Ok(Self { key, base_nonce })
    }

    /// Encrypt a single chunk with AES-256-GCM (12-byte GCM nonce derived from the
    /// base nonce XOR the part number).
    fn encrypt_chunk_internal(&self, data: &[u8], part_number: u32) -> Vec<u8> {
        use aes_gcm::aead::Aead;
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

        let cipher = Aes256Gcm::new((&self.key).into());
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&self.base_nonce[..12]);
        let n = part_number.to_le_bytes();
        nonce[0] ^= n[0];
        nonce[1] ^= n[1];
        nonce[2] ^= n[2];
        nonce[3] ^= n[3];

        cipher
            .encrypt(Nonce::from_slice(&nonce), data)
            .expect("aes256gcm encrypt")
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

// ---------------------------------------------------------------------------
// Host-provided vtable implementation (native only, not for WASM)
// ---------------------------------------------------------------------------

#[cfg(not(feature = "wasm"))]
/// Context for the custom encryptor.
struct CustomEncryptorContext {
    encryptor: Aes256GcmEncryptor,
}

#[cfg(not(feature = "wasm"))]
/// Encrypt chunk callback (called from the uploader).
unsafe extern "C" fn custom_encrypt_chunk(
    ctx: *mut c_void,
    input: *const u8,
    input_len: usize,
    part_number: u32,
) -> EncryptResult {
    let context = unsafe { &*(ctx as *const CustomEncryptorContext) };
    let data = unsafe { std::slice::from_raw_parts(input, input_len) };

    let encrypted = context.encryptor.encrypt_chunk_internal(data, part_number);

    // Hand the allocation to the uploader, which rebuilds the Vec from these
    // fields and owns it from here on (same allocator, so this is sound).
    let len = encrypted.len();
    let capacity = encrypted.capacity();
    let data_ptr = encrypted.as_ptr() as *mut u8;
    std::mem::forget(encrypted);

    EncryptResult {
        data: data_ptr,
        len,
        capacity,
    }
}

#[cfg(not(feature = "wasm"))]
/// Release a result the uploader got from `encrypt_chunk`/`compute_footer`.
/// The uploader copies the bytes out first and then hands the allocation back
/// here, so a host stays free to allocate however it likes — this is what keeps
/// the ABI usable from C/C++/Swift and not only from Rust.
unsafe extern "C" fn custom_free(_ctx: *mut c_void, data: *mut u8, _len: usize, capacity: usize) {
    unsafe {
        let _ = Vec::from_raw_parts(data, 0, capacity);
    }
}

#[cfg(not(feature = "wasm"))]
/// Destroy context callback (called when the encryptor is dropped).
unsafe extern "C" fn custom_destroy(ctx: *mut c_void) {
    unsafe {
        drop(Box::from_raw(ctx as *mut CustomEncryptorContext));
    }
}

#[cfg(not(feature = "wasm"))]
/// Descriptor callback — the bytes appended to the final data part.
///
/// Mirrors the built-in layout so a decoder needs nothing but the key/nonce:
/// the plaintext is `n_data_parts (4B LE) || effective_part_size (8B LE)`,
/// encrypted with `part_number == n_data_parts + 1`. 12 bytes → 28 bytes with
/// the AES-GCM tag. A scheme that wants no descriptor sets
/// `has_footer: false` in its vtable — this callback is then never called.
unsafe extern "C" fn custom_compute_footer(
    ctx: *mut c_void,
    n_data_parts: u32,
    effective_part_size: u64,
) -> EncryptResult {
    let context = unsafe { &*(ctx as *const CustomEncryptorContext) };

    let mut plaintext = [0u8; 12];
    plaintext[..4].copy_from_slice(&n_data_parts.to_le_bytes());
    plaintext[4..].copy_from_slice(&effective_part_size.to_le_bytes());

    let encrypted = context
        .encryptor
        .encrypt_chunk_internal(&plaintext, n_data_parts + 1);

    let len = encrypted.len();
    let capacity = encrypted.capacity();
    let data_ptr = encrypted.as_ptr() as *mut u8;
    std::mem::forget(encrypted);

    EncryptResult {
        data: data_ptr,
        len,
        capacity,
    }
}

// ---------------------------------------------------------------------------
// Registration function (called from Swift at app startup)
// Android uses JNI functions above instead.
// ---------------------------------------------------------------------------

#[cfg(all(not(feature = "wasm"), target_os = "ios"))]
/// Register the custom AES-256-GCM encryptor factory with the uploader.
///
/// No key material is passed here: the uploader calls `create` for every upload
/// with the key/nonce supplied to `encryptAndUploadFile`, so per-upload keys work
/// exactly as they do with the built-in cipher.
///
/// # Safety
/// Resolves `register_custom_encryptor` from `libuploader.framework` at link time.
#[no_mangle]
pub unsafe extern "C" fn custom_encryptor_register() {
    unsafe {
        register_custom_encryptor(&VTABLE);
    }
}

// ---------------------------------------------------------------------------
// WASM exports — compiled when `wasm` feature is enabled
// ---------------------------------------------------------------------------
//
// When built with `--features wasm --target wasm32-unknown-unknown`, this module
// exposes the same AES-256-GCM encryptor via `wasm-bindgen` so the browser can
// call it directly without a JS/WebCrypto duplicate implementation.
//
// The WASM module is loaded as a standalone crate (not linked into the uploader's
// WASM binary). The JS bridge calls `new WasmEncryptor(keyHex, nonceHex)` once,
// then uses the instance for all encrypt_chunk / compute_footer calls.
// ---------------------------------------------------------------------------

#[cfg(feature = "wasm")]
mod wasm_export {
    use super::*;
    use wasm_bindgen::prelude::*;

    /// WASM-exposed AES-256-GCM encryptor instance.
    ///
    /// Created once with the key/nonce, then reused for all part encryptions.
    /// The instance is synchronous — no async/Promise boilerplate needed.
    #[wasm_bindgen]
    pub struct WasmEncryptor {
        encryptor: Aes256GcmEncryptor,
    }

    #[wasm_bindgen]
    impl WasmEncryptor {
        /// Create a new encryptor from hex-encoded key (64 chars) and nonce (48 chars).
        ///
        /// Returns `null` on error; check the console for the error message.
        #[wasm_bindgen(constructor)]
        pub fn new(key_hex: &str, nonce_hex: &str) -> Result<WasmEncryptor, JsValue> {
            Aes256GcmEncryptor::from_hex(key_hex, nonce_hex)
                .map(|encryptor| WasmEncryptor { encryptor })
                .map_err(|e| JsValue::from_str(&e))
        }

        /// Encrypt a single chunk (part).
        ///
        /// Returns a `Uint8Array` containing the ciphertext (plaintext + 16-byte GCM tag).
        /// `part_number` is 1-based and stable across resumes.
        #[wasm_bindgen]
        pub fn encrypt_chunk(&self, input: &[u8], part_number: u32) -> Vec<u8> {
            self.encryptor.encrypt_chunk_internal(input, part_number)
        }

        /// Compute the descriptor footer appended to the final data part.
        ///
        /// Returns a `Uint8Array` containing the encrypted descriptor
        /// (12 bytes of metadata + 16-byte GCM tag = 28 bytes).
        #[wasm_bindgen]
        pub fn compute_footer(&self, n_data_parts: u32, effective_part_size: u64) -> Vec<u8> {
            let mut plaintext = [0u8; 12];
            plaintext[..4].copy_from_slice(&n_data_parts.to_le_bytes());
            plaintext[4..].copy_from_slice(&effective_part_size.to_le_bytes());
            self.encryptor
                .encrypt_chunk_internal(&plaintext, n_data_parts + 1)
        }

        /// Per-chunk ciphertext overhead (AES-GCM tag length).
        #[wasm_bindgen(getter)]
        pub fn overhead(&self) -> u32 {
            AES_GCM_TAG_LEN as u32
        }

        /// Whether the wire format ends with a descriptor.
        #[wasm_bindgen(getter)]
        pub fn has_footer(&self) -> bool {
            true
        }

        /// Descriptor size in bytes (12 bytes plaintext + 16-byte GCM tag).
        #[wasm_bindgen(getter)]
        pub fn footer_len(&self) -> u32 {
            DESCRIPTOR_LEN as u32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This mirror must stay byte-identical to the uploader's `EncryptorVTable`
    /// (`uploader/src/core/encryption.rs`). A drift here compiles and links
    /// cleanly, then misreads the table at runtime — so the offsets are asserted.
    #[test]
    fn test_vtable_layout_matches_uploader() {
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

    /// Fixture generator: writes an object exactly the way the upload engine
    /// builds one (AES-GCM parts + descriptor) using *this* crate's real
    /// encryption, so `tools/decrypt` can be checked against bytes produced by
    /// the shipped implementation instead of its own copy of the format.
    ///
    /// Opt-in, so a normal `cargo test` writes nothing:
    ///   CUSTOM_ENCRYPTOR_FIXTURE_DIR=/tmp/vec cargo test --lib write_decrypt_fixture -- --nocapture
    #[test]
    fn write_decrypt_fixture() {
        let Ok(dir) = std::env::var("CUSTOM_ENCRYPTOR_FIXTURE_DIR") else {
            return;
        };

        let encryptor = Aes256GcmEncryptor::from_hex(
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "cafebabecafebabecafebabecafebabecafebabecafebabe",
        )
        .unwrap();

        let part_size = 1024 * 1024u64; // S3 part size the engine would use
        // Plaintext bytes per part: the engine reserves the tag *and* the descriptor.
        let effective = part_size - AES_GCM_TAG_LEN as u64 - DESCRIPTOR_LEN as u64;
        let plaintext: Vec<u8> = (0..(2 * effective as usize + 4321))
            .map(|i| (i % 251) as u8)
            .collect();
        let n_data_parts = plaintext.len().div_ceil(effective as usize) as u32;

        // Descriptor: n_data_parts || effective_part_size, encrypted as part n+1.
        let mut descriptor = [0u8; 12];
        descriptor[..4].copy_from_slice(&n_data_parts.to_le_bytes());
        descriptor[4..].copy_from_slice(&effective.to_le_bytes());
        let descriptor_ct = encryptor.encrypt_chunk_internal(&descriptor, n_data_parts + 1);

        let mut object = Vec::new();
        for i in 1..=n_data_parts {
            let start = (i as u64 - 1) * effective;
            let end = ((i as u64) * effective).min(plaintext.len() as u64);
            let mut part =
                encryptor.encrypt_chunk_internal(&plaintext[start as usize..end as usize], i);
            // The descriptor rides inside the FINAL data part, exactly as the engine
            // does it: a trailing part of its own would push the real last part below
            // S3's 5 MiB minimum and the upload could never complete.
            if i == n_data_parts {
                part.extend_from_slice(&descriptor_ct);
            }
            object.extend_from_slice(&part);
        }

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(format!("{dir}/adapter-object.bin"), &object).unwrap();
        std::fs::write(format!("{dir}/adapter-plaintext.bin"), &plaintext).unwrap();
        println!(
            "fixture written: {dir}/adapter-object.bin ({} bytes, {n_data_parts} data parts, part_size={part_size})",
            object.len()
        );
    }

    #[test]
    fn test_aes256gcm_encrypt_roundtrip() {
        let encryptor = Aes256GcmEncryptor::from_hex(
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "cafebabecafebabecafebabecafebabecafebabecafebabe",
        )
        .unwrap();

        let data = b"Hello, World!";
        let encrypted = encryptor.encrypt_chunk_internal(data, 1);

        // GCM adds a 16-byte tag.
        assert_eq!(encrypted.len(), data.len() + 16);

        use aes_gcm::aead::AeadMut;
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

        let mut cipher = Aes256Gcm::new((&encryptor.key).into());
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&encryptor.base_nonce[..12]);
        let n = 1u32.to_le_bytes();
        nonce[0] ^= n[0];
        nonce[1] ^= n[1];
        nonce[2] ^= n[2];
        nonce[3] ^= n[3];

        let decrypted = cipher
            .decrypt(Nonce::from_slice(&nonce), encrypted.as_slice())
            .unwrap();
        assert_eq!(decrypted, data);
    }

    /// The descriptor must be self-describing and decryptable with only the
    /// key/nonce, i.e. `n_data_parts || effective_part_size` under part_number n+1.
    #[test]
    fn test_descriptor_part_roundtrip() {
        let encryptor = Aes256GcmEncryptor::from_hex(
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "cafebabecafebabecafebabecafebabecafebabecafebabe",
        )
        .unwrap();

        let n_data_parts = 3u32;
        let effective_part_size = 5_242_880u64;

        let ctx = Box::into_raw(Box::new(CustomEncryptorContext { encryptor }));
        let result =
            unsafe { custom_compute_footer(ctx as *mut c_void, n_data_parts, effective_part_size) };
        let footer = unsafe { std::slice::from_raw_parts(result.data, result.len) }.to_vec();
        unsafe { custom_free(ctx as *mut c_void, result.data, result.len, result.capacity) };

        assert_eq!(footer.len(), 12 + AES_GCM_TAG_LEN);
        assert_eq!(DESCRIPTOR_LEN, footer.len());

        // Decrypt with part_number == n_data_parts + 1.
        use aes_gcm::aead::AeadMut;
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
        let key = hex_to_fixed::<32>(
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "k",
        )
        .unwrap();
        let base_nonce =
            hex_to_fixed::<24>("cafebabecafebabecafebabecafebabecafebabecafebabe", "n").unwrap();
        let mut cipher = Aes256Gcm::new((&key).into());
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&base_nonce[..12]);
        let n = (n_data_parts + 1).to_le_bytes();
        nonce[0] ^= n[0];
        nonce[1] ^= n[1];
        nonce[2] ^= n[2];
        nonce[3] ^= n[3];

        let plain = cipher
            .decrypt(Nonce::from_slice(&nonce), footer.as_slice())
            .unwrap();
        assert_eq!(plain[..4], n_data_parts.to_le_bytes());
        assert_eq!(plain[4..], effective_part_size.to_le_bytes());

        unsafe { custom_destroy(ctx as *mut c_void) };
    }
}

#[cfg(all(not(feature = "wasm"), target_os = "ios"))]
extern "C" {
    /// Exported by the uploader crate (resolved from `libuploader.framework` on iOS).
    fn register_custom_encryptor(vtable: *const EncryptorVTable);
}