use std::cell::RefCell;
use std::sync::OnceLock;

use js_sys::Function;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::JsValue;

use crate::core::progress::{ProgressManager, ProgressNotifier};
use crate::core::session::{AggregateProgress, FileProgress};

static PROGRESS_MANAGER: OnceLock<ProgressManager<WasmProgressNotifier>> = OnceLock::new();

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
    fn notify(
        &self,
        fp: &FileProgress,
        session_agg: &AggregateProgress,
        transfer_agg: &AggregateProgress,
    ) {
        PROGRESS_CALLBACK.with(|cb| {
            if let Some(ref callback) = *cb.borrow() {
                let obj = js_sys::Object::new();

                // fileProgress
                let file_obj = file_progress_to_js(fp);
                js_sys::Reflect::set(&obj, &"fileProgress".into(), &file_obj).ok();

                // transferAggregate
                let t_obj = aggregate_to_js(transfer_agg);
                js_sys::Reflect::set(&obj, &"transferAggregate".into(), &t_obj).ok();

                // sessionAggregate
                let s_obj = aggregate_to_js(session_agg);
                js_sys::Reflect::set(&obj, &"sessionAggregate".into(), &s_obj).ok();

                callback.call1(&JsValue::NULL, &obj).ok();
            }
        });
    }
}

pub fn progress_manager() -> &'static ProgressManager<WasmProgressNotifier> {
    PROGRESS_MANAGER.get_or_init(|| ProgressManager::new(WasmProgressNotifier))
}

pub fn file_progress_to_js(fp: &FileProgress) -> JsValue {
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(&obj, &"fileKey".into(), &JsValue::from_str(&fp.file_key)).ok();
    js_sys::Reflect::set(
        &obj,
        &"transferId".into(),
        &JsValue::from_str(&fp.transfer_id),
    )
    .ok();
    js_sys::Reflect::set(
        &obj,
        &"totalBytes".into(),
        &JsValue::from_f64(fp.total_bytes as f64),
    )
    .ok();
    js_sys::Reflect::set(
        &obj,
        &"uploadedBytes".into(),
        &JsValue::from_f64(fp.uploaded_bytes as f64),
    )
    .ok();
    js_sys::Reflect::set(
        &obj,
        &"completedParts".into(),
        &JsValue::from(fp.completed_parts),
    )
    .ok();
    js_sys::Reflect::set(&obj, &"totalParts".into(), &JsValue::from(fp.total_parts)).ok();
    js_sys::Reflect::set(
        &obj,
        &"percentage".into(),
        &JsValue::from_f64(fp.percentage),
    )
    .ok();
    js_sys::Reflect::set(&obj, &"state".into(), &JsValue::from_str(fp.state.as_str())).ok();
    obj.into()
}

pub fn aggregate_to_js(agg: &AggregateProgress) -> JsValue {
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(
        &obj,
        &"percentage".into(),
        &JsValue::from_f64(agg.percentage),
    )
    .ok();
    js_sys::Reflect::set(
        &obj,
        &"totalSize".into(),
        &JsValue::from_f64(agg.total_size as f64),
    )
    .ok();
    js_sys::Reflect::set(
        &obj,
        &"uploadedSize".into(),
        &JsValue::from_f64(agg.uploaded_size as f64),
    )
    .ok();
    if let Some(t) = agg.total_transfers {
        js_sys::Reflect::set(&obj, &"totalTransfers".into(), &JsValue::from(t)).ok();
    }
    if let Some(c) = agg.completed_transfers {
        js_sys::Reflect::set(&obj, &"completedTransfers".into(), &JsValue::from(c)).ok();
    }
    js_sys::Reflect::set(&obj, &"totalFiles".into(), &JsValue::from(agg.total_files)).ok();
    js_sys::Reflect::set(
        &obj,
        &"completedFiles".into(),
        &JsValue::from(agg.completed_files),
    )
    .ok();
    js_sys::Reflect::set(
        &obj,
        &"state".into(),
        &JsValue::from_str(agg.state.as_str()),
    )
    .ok();
    obj.into()
}
