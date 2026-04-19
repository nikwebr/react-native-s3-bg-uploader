use std::collections::HashMap;

use wasm_bindgen::prelude::*;

use crate::core::session::{self, UploadState};
use crate::core::upload::{self, StartDecision};
use crate::wasm::api;
use crate::wasm::progress as wasmProgress;
use crate::wasm::{enqueue_file, FILE_QUEUE, PAUSE_REQUESTED, QUEUE_RUNNING};

#[wasm_bindgen(start)]
pub fn wasm_start() {
    console_error_panic_hook::set_once();
}

#[wasm_bindgen]
pub async fn wasm_load_session() {
    if let Some(s) = session::idb_load().await {
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
pub async fn wasm_start_file(
    file: web_sys::File,
    transfer_id: String,
    user_params_js: JsValue,
) -> Result<JsValue, JsValue> {
    let user_params = parse_user_params(user_params_js);
    start_file_internal(&file, &transfer_id, &user_params)
        .await
        .map(|key| JsValue::from_str(&key))
        .map_err(|e| JsValue::from_str(&format!("Start failed: {}", e)))
}

#[wasm_bindgen]
pub fn wasm_run_file(file_key: String, file: web_sys::File) {
    {
        let sess = session::session();
        if let Some(entry) = sess.files.get(&file_key) {
            if entry.state == UploadState::Completed {
                return;
            }
        } else {
            return;
        }
    }

    enqueue_file(file_key, file);
}

#[wasm_bindgen]
pub async fn upload_file(
    file: web_sys::File,
    transfer_id: String,
    user_params_js: JsValue,
) -> Result<JsValue, JsValue> {
    let user_params = parse_user_params(user_params_js);
    upload_file_internal(file, transfer_id, user_params)
        .await
        .map(|key| JsValue::from_str(&key))
        .map_err(|e| JsValue::from_str(&format!("Upload failed: {}", e)))
}

#[wasm_bindgen]
pub fn wasm_cancel_file(file_key: String) {
    FILE_QUEUE.with(|q| q.borrow_mut().retain(|e| e.file_key != file_key));
    session::session().cancel_file(&file_key);
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
    session::session().cancel_transfer(&transfer_id);
}

#[wasm_bindgen]
pub fn wasm_cancel_all() {
    FILE_QUEUE.with(|q| q.borrow_mut().clear());
    PAUSE_REQUESTED.with(|p| *p.borrow_mut() = false);
    QUEUE_RUNNING.with(|r| *r.borrow_mut() = false);
    session::clear_session();
    wasmProgress::progress_manager().clear();
}

#[wasm_bindgen]
pub fn wasm_pause_all() {
    FILE_QUEUE.with(|q| q.borrow_mut().clear());
    PAUSE_REQUESTED.with(|p| *p.borrow_mut() = true);
    session::session().pause_all();

    for file_key in &wasmProgress::progress_manager().tracked_file_keys() {
        crate::core::runtime::set_status(
            wasmProgress::progress_manager(),
            file_key,
            UploadState::Paused,
        );
    }
}

#[wasm_bindgen]
pub fn wasm_get_progress(transfer_id: JsValue, file_key: JsValue) -> JsValue {
    let tid = transfer_id.as_string().filter(|s| !s.is_empty());
    let fk = file_key.as_string().filter(|s| !s.is_empty());
    let progress = session::session().get_progress(tid.as_deref(), fk.as_deref());
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

async fn start_file_internal(
    file: &web_sys::File,
    transfer_id: &str,
    user_params: &HashMap<String, String>,
) -> Result<String, String> {
    let file_hash = crate::core::hash::sha256_web_file(file, transfer_id).await?;

    match upload::start_decision(&file_hash) {
        StartDecision::Completed { file_key } | StartDecision::Resume { file_key } => Ok(file_key),
        StartDecision::StartNew => {
            let file_size = file.size() as u64;
            let file_name = file.name();
            let start_resp =
                api::start_upload(&file_name, &file_hash, file_size, user_params).await?;
            Ok(upload::register_started_upload(
                file_hash,
                transfer_id,
                String::new(),
                file_name,
                file_size,
                user_params.clone(),
                start_resp,
            ))
        }
    }
}

async fn upload_file_internal(
    file: web_sys::File,
    transfer_id: String,
    user_params: HashMap<String, String>,
) -> Result<String, String> {
    let file_key = start_file_internal(&file, &transfer_id, &user_params).await?;

    {
        let sess = session::session();
        if let Some(entry) = sess.files.get(&file_key) {
            if entry.state == UploadState::Completed {
                return Ok(file_key.clone());
            }
        }
    }

    enqueue_file(file_key.clone(), file);
    Ok(file_key)
}
