use std::collections::HashMap;
use std::sync::Arc;

use wasm_bindgen::prelude::*;

use crate::core::encryption::XChaCha20Poly1305Encryptor;
use crate::core::runtime;
use crate::core::session::{self, register_store};
use crate::wasm::progress as wasmProgress;
use crate::wasm::store::WasmSessionStore;
use crate::wasm::{FILE_QUEUE, PAUSE_REQUESTED, PENDING_WASM_ENCRYPTION, PENDING_WASM_FILES, QUEUE_RUNNING};

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

#[wasm_bindgen]
pub fn wasm_set_config(
    start_upload_api: String,
    get_upload_urls_api: String,
    complete_api: String,
) {
    crate::core::config::set_config(&start_upload_api, &get_upload_urls_api, &complete_api);
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
    let is_running = QUEUE_RUNNING.with(|r| *r.borrow())
        && !PAUSE_REQUESTED.with(|p| *p.borrow());
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
    let encryptor = XChaCha20Poly1305Encryptor::from_hex(&key_hex, &nonce_hex)
        .map_err(|e| JsValue::from_str(&format!("Invalid encryption params: {}", e)))?;
    let hash = super::start_and_enqueue_encrypted(file, transfer_id, user_params, Arc::new(encryptor))
        .await
        .map_err(|e| JsValue::from_str(&format!("Upload failed: {}", e)))?;

    let is_running = QUEUE_RUNNING.with(|r| *r.borrow())
        && !PAUSE_REQUESTED.with(|p| *p.borrow());
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
            (String::new(), p.file_name.clone(), p.transfer_id.clone(), 0u64, 0u64)
        } else if let Some(key) = sess.hash_to_key.get(&file_hash) {
            let key = key.clone();
            if let Some(e) = sess.files.get(&key) {
                (key, e.file_name.clone(), e.transfer_id.clone(), e.total_bytes, e.uploaded_bytes)
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
    FILE_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let sess = session::session();
        q.retain(|e| {
            sess.files
                .get(&e.file_key)
                .map_or(true, |f| f.transfer_id != transfer_id)
        });
    });
    session::cancel_transfer(&transfer_id);
}

#[wasm_bindgen]
pub fn wasm_cancel_all() {
    FILE_QUEUE.with(|q| q.borrow_mut().clear());
    PENDING_WASM_FILES.with(|m| m.borrow_mut().clear());
    PENDING_WASM_ENCRYPTION.with(|m| m.borrow_mut().clear());
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
    runtime::resume_all(wasmProgress::progress_manager())
        .map_err(|e| JsValue::from_str(&e))?;
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
