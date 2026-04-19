use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use crate::core::api;
use crate::core::hash::sha256_file;
use crate::core::runtime;
use crate::core::session::{self, UploadState};
use crate::core::upload::{self, StartDecision};
use crate::ios::progress as iosProgress;
use crate::ios::{enqueue_key, init_nyquest, PAUSE_FLAG, QUEUE};

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

    match start_upload_and_enqueue(&path, &tid, user_params) {
        Ok(file_key) => CString::new(file_key)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        Err(e) => {
            eprintln!("upload_file failed: {}", e);
            std::ptr::null_mut()
        }
    }
}

fn start_upload_and_enqueue(
    file_path: &str,
    transfer_id: &str,
    user_params: HashMap<String, String>,
) -> Result<String, Box<dyn std::error::Error>> {
    init_nyquest();

    let file_hash = sha256_file(file_path, transfer_id)?;

    match upload::start_decision(&file_hash) {
        StartDecision::Completed { file_key } => return Ok(file_key),
        StartDecision::Resume { file_key } => {
            session::session().update_file_path(&file_key, file_path.to_string());
            enqueue_key(file_key.clone());
            return Ok(file_key);
        }
        StartDecision::StartNew => {}
    }

    let client = nyquest::ClientBuilder::default()
        .request_timeout(std::time::Duration::from_secs(30))
        .build_blocking()
        .map_err(|e| format!("Failed to create client: {:?}", e))?;

    let file_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let file_size = std::fs::metadata(file_path)?.len();

    let start_resp = api::start_upload(&client, &file_name, &file_hash, file_size, &user_params)?;
    let file_key = upload::register_started_upload(
        file_hash,
        transfer_id,
        file_path.to_string(),
        file_name,
        file_size,
        user_params,
        start_resp,
    );
    session::persist_session();

    enqueue_key(file_key.clone());
    Ok(file_key)
}

#[no_mangle]
pub extern "C" fn cancel_file(file_key: *const c_char) {
    if file_key.is_null() {
        return;
    }
    let key = unsafe { CStr::from_ptr(file_key) }.to_str().unwrap_or("");
    session::session().cancel_file(key);
    session::persist_session();
}

#[no_mangle]
pub extern "C" fn cancel_transfer(transfer_id: *const c_char) {
    if transfer_id.is_null() {
        return;
    }
    let tid = unsafe { CStr::from_ptr(transfer_id) }
        .to_str()
        .unwrap_or("");
    session::session().cancel_transfer(tid);
    session::persist_session();
}

#[no_mangle]
pub extern "C" fn cancel_all() {
    session::clear_session();
}

#[no_mangle]
pub extern "C" fn pause_all() {
    PAUSE_FLAG.store(true, std::sync::atomic::Ordering::Relaxed);
    session::session().pause_all();
    session::persist_session();

    for file_key in &iosProgress::progress_manager().tracked_file_keys() {
        runtime::set_status(
            iosProgress::progress_manager(),
            file_key,
            UploadState::Paused,
        );
    }
}

#[no_mangle]
pub extern "C" fn resume_all() {
    PAUSE_FLAG.store(false, std::sync::atomic::Ordering::Relaxed);
    QUEUE.lock().unwrap().pending.clear();
    if let Some(next) = session::session().next_pending_file() {
        enqueue_key(next);
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
