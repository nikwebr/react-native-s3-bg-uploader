use std::sync::OnceLock;
use std::cell::RefCell;
use js_sys::Function;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::core::progress::{ProgressManager, ProgressNotifier, UploadProgress};
use crate::wasm::progress;

static PROGRESS_MANAGER: OnceLock<ProgressManager<WasmProgressNotifier>> = OnceLock::new();

#[wasm_bindgen]
pub fn get_upload_progress() -> f64 {
    progress::progress_manager().percentage()
}

#[wasm_bindgen]
pub fn get_upload_progress_json() -> JsValue {
    if let Some(progress) = progress::progress_manager().snapshot() {
        return progress::progress_to_js_object(&progress);
    }

    JsValue::NULL
}

thread_local! {
    static PROGRESS_CALLBACK: RefCell<Option<Function>> = RefCell::new(None);
}

#[wasm_bindgen]
pub fn set_progress_callback(callback: Option<Function>) {
    PROGRESS_CALLBACK.with(|cb| {
        *cb.borrow_mut() = callback;
    });
}

pub struct WasmProgressNotifier;

impl ProgressNotifier for WasmProgressNotifier {
    fn notify(&self, progress: &UploadProgress) {
        notify_progress_wasm(progress);
    }
}

pub fn progress_manager() -> &'static ProgressManager<WasmProgressNotifier> {
    PROGRESS_MANAGER.get_or_init(|| ProgressManager::new(WasmProgressNotifier))
}

pub fn progress_to_js_object(progress: &UploadProgress) -> JsValue {
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(&obj, &"totalBytes".into(), &JsValue::from(progress.total_bytes)).ok();
    js_sys::Reflect::set(&obj, &"uploadedBytes".into(), &JsValue::from(progress.uploaded_bytes())).ok();
    js_sys::Reflect::set(&obj, &"completedParts".into(), &JsValue::from(progress.completed_parts)).ok();
    js_sys::Reflect::set(&obj, &"totalParts".into(), &JsValue::from(progress.total_parts)).ok();
    js_sys::Reflect::set(&obj, &"percentage".into(), &JsValue::from(progress.percentage())).ok();
    js_sys::Reflect::set(&obj, &"state".into(), &JsValue::from_str(progress.status.as_str())).ok();
    obj.into()
}

fn notify_progress_wasm(progress: &UploadProgress) {
    PROGRESS_CALLBACK.with(|cb| {
        if let Some(ref callback) = *cb.borrow() {
            // Progress-Objekt für JavaScript erstellen
            let obj = progress_to_js_object(progress);
            // Callback aufrufen
            callback.call1(&JsValue::NULL, &obj).ok();
        }
    });
}