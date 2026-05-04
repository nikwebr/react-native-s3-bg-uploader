use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use crate::core::runtime;
use crate::core::session::{self, register_store, UploadState};
use crate::ios::progress as iosProgress;
use crate::ios::{PAUSE_FLAG, QUEUE};
use crate::native::NativeSessionStore;

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

/// Phase 1: hash file and pre-register in session. Returns file hash as C string (caller frees).
#[no_mangle]
pub extern "C" fn hash_and_pre_register(
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

    match super::hash_and_pre_register(&path, &tid, user_params) {
        Ok(file_hash) => {
            CString::new(file_hash)
                .map(|s| s.into_raw())
                .unwrap_or(std::ptr::null_mut())
        }
        Err(e) => {
            CString::new(format!("ERROR:{}", e))
                .map(|s| s.into_raw())
                .unwrap_or(std::ptr::null_mut())
        }
    }
}

/// Phase 2: call start_api and enqueue the file for upload.
/// Returns file key as C string (caller frees).
#[no_mangle]
pub extern "C" fn initialize_file(
    file_hash: *const c_char,
    transfer_id: *const c_char,
) -> *mut c_char {
    if file_hash.is_null() || transfer_id.is_null() {
        return std::ptr::null_mut();
    }

    let hash = unsafe { CStr::from_ptr(file_hash) }
        .to_str()
        .unwrap_or("")
        .to_string();
    let tid = unsafe { CStr::from_ptr(transfer_id) }
        .to_str()
        .unwrap_or("")
        .to_string();

    match super::initialize_and_enqueue(&hash, &tid) {
        Ok(file_key) => {
            runtime::notify_file_registered(iosProgress::progress_manager(), &file_key);
            CString::new(file_key)
                .map(|s| s.into_raw())
                .unwrap_or(std::ptr::null_mut())
        }
        Err(e) => {
            eprintln!("initialize_file failed: {}", e);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn cancel_file(file_hash: *const c_char) {
    if file_hash.is_null() {
        return;
    }
    let hash = unsafe { CStr::from_ptr(file_hash) }.to_str().unwrap_or("");

    let (file_key, file_name, transfer_id, total_bytes, uploaded_bytes) = {
        let sess = session::session();
        if let Some(p) = sess.pending_files.get(hash) {
            (String::new(), p.file_name.clone(), p.transfer_id.clone(), 0u64, 0u64)
        } else if let Some(key) = sess.hash_to_key.get(hash) {
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
        iosProgress::progress_manager().remove(&file_key);
    }
    session::cancel_file_by_hash(hash);

    if !transfer_id.is_empty() {
        runtime::notify_cancelled_file(
            iosProgress::progress_manager(),
            &file_key,
            &file_name,
            hash,
            &transfer_id,
            total_bytes,
            uploaded_bytes,
        );
    }
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
    iosProgress::progress_manager().clear();
    session::clear_session();
}

#[no_mangle]
pub extern "C" fn pause_all() {
    PAUSE_FLAG.store(true, std::sync::atomic::Ordering::Relaxed);
    runtime::pause_all(iosProgress::progress_manager());
}

#[no_mangle]
pub extern "C" fn resume_all() -> *mut c_char {
    // Reject if any persisted file hasn't been re-provided yet.
    {
        let sess = session::session();
        let missing = sess.files_needing_provision.len();
        if missing > 0 {
            let msg = format!(
                "Cannot resume: {missing} file(s) not yet re-provided. Call uploadFile() for each missing file first."
            );
            return CString::new(msg)
                .map(|s| s.into_raw())
                .unwrap_or(std::ptr::null_mut());
        }
    }
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
    let next_key = session::session().next_pending_file();
    if let Some(file_key) = next_key {
        crate::ios::enqueue_key(file_key);
    }
    std::ptr::null_mut()
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
pub extern "C" fn get_global_state() -> *mut c_char {
    let state = session::session().global_state.as_str().to_string();
    CString::new(state)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) };
    }
}
