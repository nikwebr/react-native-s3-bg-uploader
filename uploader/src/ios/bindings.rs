use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use crate::native::NativeSessionStore;
use crate::core::runtime;
use crate::core::session::{self, register_store, UploadState};
use crate::ios::progress as iosProgress;
use crate::ios::{PAUSE_FLAG, QUEUE};

fn ensure_store() {
    register_store(NativeSessionStore);
}

#[no_mangle]
pub extern "C" fn add(one: i32, two: i32) -> i32 {
    one + two
}

#[no_mangle]
pub extern "C" fn set_config(
    start_upload_api: *const c_char,
    get_upload_urls_api: *const c_char,
    complete_api: *const c_char,
) {
    ensure_store();
    let s = unsafe { CStr::from_ptr(start_upload_api) }
        .to_str()
        .unwrap_or("");
    let g = unsafe { CStr::from_ptr(get_upload_urls_api) }
        .to_str()
        .unwrap_or("");
    let c = unsafe { CStr::from_ptr(complete_api) }
        .to_str()
        .unwrap_or("");
    crate::core::config::set_config(s, g, c);
}

#[no_mangle]
pub extern "C" fn set_storage_path(path: *const c_char) {
    ensure_store();
    let p = unsafe { CStr::from_ptr(path) }.to_str().unwrap_or("");
    session::set_storage_path(p);
}

#[no_mangle]
pub extern "C" fn upload_file(
    file_path: *const c_char,
    transfer_id: *const c_char,
    user_params_json: *const c_char,
) -> *mut c_char {
    if file_path.is_null() || transfer_id.is_null() {
        return std::ptr::null_mut();
    }

    let path = unsafe { CStr::from_ptr(file_path) }
        .to_str()
        .unwrap_or("")
        .to_string();
    let tid = unsafe { CStr::from_ptr(transfer_id) }
        .to_str()
        .unwrap_or("")
        .to_string();

    let user_params: HashMap<String, String> = if user_params_json.is_null() {
        HashMap::new()
    } else {
        let json_str = unsafe { CStr::from_ptr(user_params_json) }
            .to_str()
            .unwrap_or("{}");
        serde_json::from_str(json_str).unwrap_or_default()
    };

    match super::start_and_enqueue(&path, &tid, user_params) {
        Ok(file_key) => CString::new(file_key)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        Err(e) => {
            eprintln!("upload_file failed: {}", e);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn cancel_file(file_key: *const c_char) {
    if file_key.is_null() {
        return;
    }
    let key = unsafe { CStr::from_ptr(file_key) }.to_str().unwrap_or("");
    session::cancel_file(key);
}

#[no_mangle]
pub extern "C" fn cancel_transfer(transfer_id: *const c_char) {
    if transfer_id.is_null() {
        return;
    }
    let tid = unsafe { CStr::from_ptr(transfer_id) }
        .to_str()
        .unwrap_or("");
    session::cancel_transfer(tid);
}

#[no_mangle]
pub extern "C" fn cancel_all() {
    session::clear_session();
}

#[no_mangle]
pub extern "C" fn pause_all() {
    PAUSE_FLAG.store(true, std::sync::atomic::Ordering::Relaxed);
    runtime::pause_all(iosProgress::progress_manager());
}

#[no_mangle]
pub extern "C" fn resume_all() {
    let resumed_keys: Vec<String> = {
        let sess = session::session();
        sess.files
            .values()
            .filter(|e| e.state == UploadState::Paused || e.state == UploadState::Failed)
            .map(|e| e.file_key.clone())
            .collect()
    };
    PAUSE_FLAG.store(false, std::sync::atomic::Ordering::Relaxed);
    session::resume_all();
    for key in &resumed_keys {
        iosProgress::progress_manager().remove(key);
    }
    let mut queue = QUEUE.lock().unwrap();
    queue.pending.clear();
    queue.pending_keys.clear();
    drop(queue);
    if let Some(file_key) = session::session().next_pending_file() {
        crate::ios::enqueue_key(file_key);
    }
}

#[no_mangle]
pub extern "C" fn get_progress_json(
    transfer_id: *const c_char,
    file_key: *const c_char,
) -> *mut c_char {
    let tid = if transfer_id.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(transfer_id) }.to_str().ok()
    };
    let fk = if file_key.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(file_key) }.to_str().ok()
    };

    let progress = session::session().get_progress(tid, fk);
    let json: Vec<serde_json::Value> = progress.iter().map(|p| p.to_json()).collect();
    let s = serde_json::to_string(&json).unwrap_or_else(|_| "[]".to_string());
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn get_aggregate_progress_json(transfer_id: *const c_char) -> *mut c_char {
    let tid = if transfer_id.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(transfer_id) }.to_str().ok()
    };
    let agg = session::session().get_aggregate_progress(tid);
    let s = serde_json::to_string(&agg.to_json()).unwrap_or_else(|_| "{}".to_string());
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn set_task_title(title: *const c_char) {
    if title.is_null() {
        return;
    }
    let t = unsafe { CStr::from_ptr(title) }.to_str().unwrap_or("");
    session::session().title_template = t.to_string();
}

#[no_mangle]
pub extern "C" fn set_task_subtitle(subtitle: *const c_char) {
    if subtitle.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(subtitle) }.to_str().unwrap_or("");
    session::session().subtitle_template = s.to_string();
}

#[no_mangle]
pub extern "C" fn format_title_string() -> *mut c_char {
    let s = session::session().format_title();
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn format_subtitle_string() -> *mut c_char {
    let s = session::session().format_subtitle();
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) };
    }
}
