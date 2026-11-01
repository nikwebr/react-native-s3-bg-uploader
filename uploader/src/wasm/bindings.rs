use std::collections::HashMap;
use std::os::raw::c_void;
use std::sync::Arc;

use wasm_bindgen::prelude::*;

use crate::core::encryption::{self, ChunkEncryptor, XChaCha20Poly1305Encryptor};
use crate::core::runtime;
use crate::core::session::{self, register_store};
use crate::wasm::progress as wasmProgress;
use crate::wasm::store::WasmSessionStore;
use crate::wasm::{
    FILE_QUEUE, PAUSE_REQUESTED, PENDING_WASM_ENCRYPTION, PENDING_WASM_FILES, QUEUE_RUNNING,
};

#[wasm_bindgen(start)]
pub fn wasm_start() {
    console_error_panic_hook::set_once();
    register_store(WasmSessionStore);
}

#[wasm_bindgen]
pub async fn wasm_load_session() {
    if let Some(mut s) = crate::wasm::store::load().await {
        s.recompute_needs_provision();
        *session::session() = s;
    }
}

#[wasm_bindgen]
pub fn add(one: f64, two: f64) -> f64 {
    one + two
}

// ===========================================================================
//  Custom encryptor registration (WASM — same C FFI as native)
// ===========================================================================

/// Register a host-provided encryptor from the WASM side.
///
/// This is the **same stable C FFI function** used by native platforms.
/// The user's Rust crate (statically linked into the WASM binary)
/// calls this function at library load time with their vtable pointer.
///
/// # Example (user's Rust crate)
/// ```rust
/// use std::os::raw::c_void;
/// use s3_bg_uploader::core::encryption::{EncryptResult, EncryptorVTable};
///
/// #[no_mangle]
/// pub extern "C" fn my_encrypt(ctx: *mut c_void,
///                              input: *const u8,
///                              input_len: usize,
///                              part_number: u32) -> EncryptResult {
///     // ... encrypt data (derive the per-part nonce from part_number so a
///     //     resumed upload re-encrypts a part identically) ...
/// }
///
/// /// Optional descriptor part — only called when `has_footer` is true.
/// #[no_mangle]
/// pub extern "C" fn my_descriptor(ctx: *mut c_void,
///                                 n_data_parts: u32,
///                                 effective_part_size: u64) -> EncryptResult {
///     // ... e.g. encrypt `n_data_parts || effective_part_size` ...
/// }
///
/// // At library init:
/// static VTABLE: EncryptorVTable = EncryptorVTable {
///     create: my_create,        // per-upload context from the upload's key
///     encrypt_chunk: my_encrypt,
///     free: my_free,
///     destroy: my_destroy,
///     ciphertext_overhead: 16,  // bytes your scheme adds per chunk (tag/IV/…)
///     has_footer: false,        // true => append a descriptor to the last data part
///     footer_len: 0,            // descriptor bytes; non-zero when has_footer is true
///     compute_footer: my_descriptor,
/// };
///
/// #[no_mangle]
/// pub extern "C" fn init_encryptor() {
///     s3_bg_uploader::register_custom_encryptor(&VTABLE);
/// }
/// ```
#[no_mangle]
pub extern "C" fn register_custom_encryptor(vtable: *const encryption::EncryptorVTable) {
    encryption::register_custom_encryptor(vtable);
}

/// Register a custom encryptor implemented in **JavaScript** (web only).
///
/// On web the customer's mechanism runs in the same worker as the WASM module,
/// so it is registered as callbacks instead of a C vtable:
///
/// ```js
/// wasm.wasm_register_js_encryptor(
///   16,      // overhead: bytes added per part
///   true,    // has_footer: the format ends with a descriptor
///   28,      // footer_len: descriptor size inside the last data part
///   (input, partNumber) => crypto.subtle.encrypt(...),   // may return a Promise
///   (nDataParts, effectivePartSize) => crypto.subtle.encrypt(...),
/// );
/// ```
///
/// The callbacks receive a `Uint8Array` and must return a `Uint8Array` or
/// `ArrayBuffer` (sync or via a Promise — browser crypto is async). `part_number`
/// is 1-based and stable across resumes, so a resumed part re-encrypts to the
/// same bytes. A JS encryptor takes precedence over a statically linked one and
/// over the built-in config, but only for files the app marked as encrypted.
#[wasm_bindgen]
pub fn wasm_register_js_encryptor(
    overhead: u32,
    has_footer: bool,
    footer_len: u32,
    create: js_sys::Function,
) -> Result<(), JsValue> {
    crate::wasm::js_encryptor::register(overhead, has_footer, footer_len, create)
        .map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn wasm_set_config(
    start_upload_api: String,
    get_upload_urls_api: String,
    complete_api: String,
    send_file_hash: bool,
) {
    crate::core::config::set_config(
        &start_upload_api,
        &get_upload_urls_api,
        &complete_api,
        send_file_hash,
    );
}

#[wasm_bindgen]
pub async fn upload_file(
    file: web_sys::File,
    transfer_id: String,
    user_params_js: JsValue,
) -> Result<JsValue, JsValue> {
    let user_params = parse_user_params(user_params_js);
    let hash = super::start_and_enqueue(file, transfer_id, user_params)
        .await
        .map_err(|e| JsValue::from_str(&format!("Upload failed: {}", e)))?;

    // If the queue is already running, process the newly pending file immediately
    // so it is enqueued without waiting for an explicit resume() call.
    let is_running = QUEUE_RUNNING.with(|r| *r.borrow()) && !PAUSE_REQUESTED.with(|p| *p.borrow());
    if is_running {
        crate::wasm::process_pending_files();
    }

    Ok(JsValue::from_str(&hash))
}

/// Encrypted upload: same as `upload_file` but encrypts each chunk with XChaCha20-Poly1305.
/// `key_hex` = 64 hex chars (32 bytes), `nonce_hex` = 48 hex chars (24 bytes).
#[wasm_bindgen]
pub async fn upload_file_encrypted(
    file: web_sys::File,
    transfer_id: String,
    key_hex: String,
    nonce_hex: String,
    user_params_js: JsValue,
) -> Result<JsValue, JsValue> {
    let user_params = parse_user_params(user_params_js);
    let hash = super::start_and_enqueue_encrypted(
        file,
        transfer_id,
        user_params,
        key_hex,
        nonce_hex,
    )
    .await
            .map_err(|e| JsValue::from_str(&format!("Upload failed: {}", e)))?;

    let is_running = QUEUE_RUNNING.with(|r| *r.borrow()) && !PAUSE_REQUESTED.with(|p| *p.borrow());
    if is_running {
        crate::wasm::process_pending_files();
    }

    Ok(JsValue::from_str(&hash))
}

#[wasm_bindgen]
pub fn wasm_cancel_file(file_hash: String) {
    let (file_key, file_name, transfer_id, total_bytes, uploaded_bytes) = {
        let sess = session::session();
        if let Some(p) = sess.pending_files.get(&file_hash) {
            (
                String::new(),
                p.file_name.clone(),
                p.transfer_id.clone(),
                0u64,
                0u64,
            )
        } else if let Some(key) = sess.hash_to_key.get(&file_hash) {
            let key = key.clone();
            if let Some(e) = sess.files.get(&key) {
                (
                    key,
                    e.file_name.clone(),
                    e.transfer_id.clone(),
                    e.total_bytes,
                    e.uploaded_bytes,
                )
            } else {
                (key, String::new(), String::new(), 0, 0)
            }
        } else {
            (String::new(), String::new(), String::new(), 0, 0)
        }
    };

    if !file_key.is_empty() {
        FILE_QUEUE.with(|q| q.borrow_mut().retain(|e| e.file_key != file_key));
        wasmProgress::progress_manager().remove(&file_key);
    }
    PENDING_WASM_FILES.with(|m| m.borrow_mut().remove(&file_hash));
    PENDING_WASM_ENCRYPTION.with(|m| m.borrow_mut().remove(&file_hash));
    crate::wasm::js_encryptor::forget_instance(&file_hash);
    session::cancel_file_by_hash(&file_hash);

    if !transfer_id.is_empty() {
        runtime::notify_cancelled_file(
            wasmProgress::progress_manager(),
            &file_key,
            &file_name,
            &file_hash,
            &transfer_id,
            total_bytes,
            uploaded_bytes,
        );
    }
}

#[wasm_bindgen]
pub fn wasm_cancel_transfer(transfer_id: String) {
    let hashes: Vec<String> = {
        let sess = session::session();
        sess.files
            .values()
            .filter(|e| e.transfer_id == transfer_id)
            .map(|e| e.file_hash.clone())
            .chain(
                sess.pending_files
                    .values()
                    .filter(|e| e.transfer_id == transfer_id)
                    .map(|e| e.file_hash.clone()),
            )
            .collect()
    };
    for hash in hashes {
        wasm_cancel_file(hash);
    }
    session::cancel_transfer(&transfer_id);
}

#[wasm_bindgen]
pub fn wasm_cancel_all() {
    FILE_QUEUE.with(|q| q.borrow_mut().clear());
    PENDING_WASM_FILES.with(|m| m.borrow_mut().clear());
    PENDING_WASM_ENCRYPTION.with(|m| m.borrow_mut().clear());
    crate::wasm::js_encryptor::forget_all_instances();
    PAUSE_REQUESTED.with(|p| *p.borrow_mut() = false);
    QUEUE_RUNNING.with(|r| *r.borrow_mut() = false);
    session::clear_session();
    wasmProgress::progress_manager().clear();
}

#[wasm_bindgen]
pub fn wasm_pause_all() {
    PAUSE_REQUESTED.with(|p| *p.borrow_mut() = true);
    crate::core::runtime::pause_all(wasmProgress::progress_manager());
}

#[wasm_bindgen]
pub fn wasm_resume_all() -> Result<(), JsValue> {
    runtime::resume_all(wasmProgress::progress_manager()).map_err(|e| JsValue::from_str(&e))?;
    crate::wasm::process_pending_files();
    crate::wasm::resume_queue();
    Ok(())
}

#[wasm_bindgen]
pub fn wasm_get_progress(transfer_id: JsValue, file_hash: JsValue) -> JsValue {
    let tid = transfer_id.as_string().filter(|s| !s.is_empty());
    let fh = file_hash.as_string().filter(|s| !s.is_empty());
    let progress = session::session().get_progress(tid.as_deref(), fh.as_deref());
    let arr = js_sys::Array::new();
    for p in &progress {
        let json_str = serde_json::to_string(&p.to_json()).unwrap_or_default();
        if let Ok(js_obj) = js_sys::JSON::parse(&json_str) {
            arr.push(&js_obj);
        }
    }
    arr.into()
}

#[wasm_bindgen]
pub fn wasm_get_aggregate_progress(transfer_id: JsValue) -> JsValue {
    let tid = transfer_id.as_string().filter(|s| !s.is_empty());
    let agg = session::session().get_aggregate_progress(tid.as_deref());
    let json_str = serde_json::to_string(&agg.to_json()).unwrap_or_else(|_| "{}".to_string());
    js_sys::JSON::parse(&json_str).unwrap_or(JsValue::NULL)
}

fn parse_user_params(user_params_js: JsValue) -> HashMap<String, String> {
    if user_params_js.is_null() || user_params_js.is_undefined() {
        HashMap::new()
    } else {
        let json_str = js_sys::JSON::stringify(&user_params_js)
            .ok()
            .and_then(|s| s.as_string())
            .unwrap_or_else(|| "{}".to_string());
        serde_json::from_str(&json_str).unwrap_or_default()
    }
}
