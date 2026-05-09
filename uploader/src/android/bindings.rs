use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use jni::objects::{GlobalRef, JClass, JString, JValue};
use jni::sys::{jint, jstring};
use jni::{JavaVM, JNIEnv};

use crate::android::progress as androidProgress;
use crate::android::{enqueue_front, PAUSE_FLAG, QUEUE_SIGNAL};
use crate::native::NativeSessionStore;
use crate::core::runtime;
use crate::core::session::{self, register_store, format_template, GlobalUploaderState, UploadState};

static JVM: std::sync::OnceLock<Arc<JavaVM>> = std::sync::OnceLock::new();
// Cached from the JNI-thread that has the app class loader. Background threads
// attached via attach_current_thread_as_daemon use the bootstrap class loader
// and cannot find app classes by name.
static CLASS_HYBRID: std::sync::OnceLock<GlobalRef> = std::sync::OnceLock::new();

fn ensure_store() {
    register_store(NativeSessionStore);
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeInitProgressCallback(
    env: JNIEnv,
    class: JClass,
) {
    let vm = match env.get_java_vm() {
        Ok(v) => Arc::new(v),
        Err(_) => return,
    };
    let _ = JVM.set(vm);
    let vm = match JVM.get() {
        Some(v) => v.clone(),
        None => return,
    };
    if CLASS_HYBRID.get().is_none() {
        if let Ok(global) = env.new_global_ref(class) {
            let _ = CLASS_HYBRID.set(global);
        }
    }
    androidProgress::set_progress_callback(Some(Box::new(move |
        file_key, file_name, file_hash, transfer_id,
        total_bytes, uploaded_bytes, completed_parts, total_parts, percentage, state,
        transfer_pct, transfer_total_size, transfer_uploaded_size,
        transfer_total_files, transfer_completed_files, transfer_state,
        session_pct, session_total_size, session_uploaded_size,
        session_total_transfers, session_completed_transfers,
        session_total_files, session_completed_files, session_state,
    | {
        let mut file_obj = serde_json::json!({
            "fileName": file_name,
            "fileHash": file_hash,
            "transferId": transfer_id,
            "totalBytes": total_bytes,
            "uploadedBytes": uploaded_bytes,
            "completedParts": completed_parts,
            "totalParts": total_parts,
            "percentage": percentage,
            "state": state,
        });
        if !file_key.is_empty() {
            file_obj["fileKey"] = serde_json::Value::String(file_key.to_string());
        }
        let json = serde_json::json!({
            "file": file_obj,
            "sessionAgg": {
                "percentage": session_pct,
                "totalSize": session_total_size,
                "uploadedSize": session_uploaded_size,
                "totalTransfers": session_total_transfers,
                "completedTransfers": session_completed_transfers,
                "totalFiles": session_total_files,
                "completedFiles": session_completed_files,
                "state": session_state,
            },
            "transferAgg": {
                "percentage": transfer_pct,
                "totalSize": transfer_total_size,
                "uploadedSize": transfer_uploaded_size,
                "totalFiles": transfer_total_files,
                "completedFiles": transfer_completed_files,
                "state": transfer_state,
            },
        }).to_string();
        let Ok(mut env) = vm.attach_current_thread_as_daemon() else { return };
        let Ok(json_jstr) = env.new_string(&json) else { return };
        let Some(class_ref) = CLASS_HYBRID.get() else { return };
        // Safety: class_ref is a GlobalRef to HybridS3BgUploader.class, stored
        // when called from the JNI thread that has the correct app class loader.
        let class_jclass: JClass<'_> = unsafe { JClass::from_raw(class_ref.as_raw()) };
        let _ = env.call_static_method(
            class_jclass,
            "onNativeProgress",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&*json_jstr)],
        );
        let _ = env.exception_clear();
    })));
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeSetConfig(
    mut env: JNIEnv,
    _class: JClass,
    start_upload_api: JString,
    get_upload_urls_api: JString,
    complete_api: JString,
) {
    ensure_store();
    let s: String = env
        .get_string(&start_upload_api)
        .map(|s| s.into())
        .unwrap_or_default();
    let g: String = env
        .get_string(&get_upload_urls_api)
        .map(|s| s.into())
        .unwrap_or_default();
    let c: String = env
        .get_string(&complete_api)
        .map(|s| s.into())
        .unwrap_or_default();
    crate::core::config::set_config(&s, &g, &c);
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeSetStoragePath(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
) {
    ensure_store();
    let p: String = env.get_string(&path).map(|s| s.into()).unwrap_or_default();
    session::set_storage_path(&p);
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeSetTaskTitle(
    mut env: JNIEnv,
    _class: JClass,
    title: JString,
) {
    let t: String = env.get_string(&title).map(|s| s.into()).unwrap_or_default();
    session::session().title_template = t;
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeSetTaskSubtitle(
    mut env: JNIEnv,
    _class: JClass,
    subtitle: JString,
) {
    let s: String = env
        .get_string(&subtitle)
        .map(|s| s.into())
        .unwrap_or_default();
    session::session().subtitle_template = s;
}

/// Phase 1: hash fd and pre-register. Returns file hash as JString.
#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeHashAndPreRegister(
    mut env: JNIEnv,
    _class: JClass,
    fd: jint,
    file_name: JString,
    transfer_id: JString,
    user_params_json: JString,
) -> jstring {
    let name: String = env
        .get_string(&file_name)
        .map(|s| s.into())
        .unwrap_or_else(|_| "file".to_string());
    let tid: String = env
        .get_string(&transfer_id)
        .map(|s| s.into())
        .unwrap_or_default();
    let params_str: String = env
        .get_string(&user_params_json)
        .map(|s| s.into())
        .unwrap_or_else(|_| "{}".to_string());
    let user_params: HashMap<String, String> =
        serde_json::from_str(&params_str).unwrap_or_default();

    match super::hash_and_pre_register(fd as RawFd, name, &tid, user_params) {
        Ok(file_hash) => {
            env.new_string(&file_hash)
                .map(|s| s.into_raw())
                .unwrap_or(empty_jstring(&mut env))
        }
        Err(e) => {
            env.new_string(format!("ERROR:{}", e))
                .map(|s| s.into_raw())
                .unwrap_or(empty_jstring(&mut env))
        }
    }
}

/// Phase 2: call start_api and enqueue the file. Returns file key as JString.
#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeInitializeFile(
    mut env: JNIEnv,
    _class: JClass,
    fd: jint,
    file_hash: JString,
    transfer_id: JString,
) -> jstring {
    let hash: String = env
        .get_string(&file_hash)
        .map(|s| s.into())
        .unwrap_or_default();
    let tid: String = env
        .get_string(&transfer_id)
        .map(|s| s.into())
        .unwrap_or_default();

    match super::initialize_and_enqueue(fd as RawFd, &hash, &tid) {
        Ok(file_key) => {
            runtime::notify_file_registered(androidProgress::progress_manager(), &file_key);
            env.new_string(&file_key)
                .map(|s| s.into_raw())
                .unwrap_or(empty_jstring(&mut env))
        }
        Err(e) => {
            log::error!("nativeInitializeFile failed: {}", e);
            empty_jstring(&mut env)
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetFormattedTitle(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let template = session::session().title_template.clone();
    let s = format_with_live_aggregate(template);
    env.new_string(&s)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetFormattedSubtitle(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let template = session::session().subtitle_template.clone();
    let s = format_with_live_aggregate(template);
    env.new_string(&s)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
}

fn format_with_live_aggregate(template: String) -> String {
    if template.is_empty() {
        return String::new();
    }
    let (session_agg, transfer_agg, current_tid) = {
        let sess = session::session();
        let s = sess.get_aggregate_progress(None);
        let t = sess.get_aggregate_progress(sess.current_transfer_id.as_deref());
        let ctid = sess.current_transfer_id.clone().unwrap_or_default();
        (s, t, ctid)
    };
    let (live_agg, _) = androidProgress::progress_manager().get_live_aggregate(
        session_agg,
        transfer_agg,
        &current_tid,
    );
    format_template(&template, &live_agg)
}

fn empty_jstring(env: &mut JNIEnv) -> jstring {
    env.new_string("")
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeCancelFile(
    mut env: JNIEnv,
    _class: JClass,
    file_hash: JString,
) {
    let hash: String = env
        .get_string(&file_hash)
        .map(|s| s.into())
        .unwrap_or_default();

    let (file_key, file_name, transfer_id, total_bytes, uploaded_bytes) = {
        let sess = session::session();
        if let Some(p) = sess.pending_files.get(&hash) {
            (String::new(), p.file_name.clone(), p.transfer_id.clone(), 0u64, 0u64)
        } else if let Some(key) = sess.hash_to_key.get(&hash) {
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
        androidProgress::progress_manager().remove(&file_key);
        // Close any saved fd so it doesn't get re-enqueued on resume.
        if let Some(fd) = crate::android::FAILED_FDS.lock().unwrap().remove(&file_key) {
            unsafe { libc::close(fd) };
        }
    }
    session::cancel_file_by_hash(&hash);

    if !transfer_id.is_empty() {
        runtime::notify_cancelled_file(
            androidProgress::progress_manager(),
            &file_key,
            &file_name,
            &hash,
            &transfer_id,
            total_bytes,
            uploaded_bytes,
        );
    }
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeCancelTransfer(
    mut env: JNIEnv,
    _class: JClass,
    transfer_id: JString,
) {
    let t: String = env
        .get_string(&transfer_id)
        .map(|s| s.into())
        .unwrap_or_default();
    // Close saved fds for any failed files in this transfer.
    {
        let keys: Vec<String> = session::session()
            .files
            .values()
            .filter(|e| e.transfer_id == t)
            .map(|e| e.file_key.clone())
            .collect();
        let mut failed = crate::android::FAILED_FDS.lock().unwrap();
        for k in &keys {
            if let Some(fd) = failed.remove(k) {
                unsafe { libc::close(fd) };
            }
        }
    }
    session::cancel_transfer(&t);
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeCancelAll(
    _env: JNIEnv,
    _class: JClass,
) {
    androidProgress::progress_manager().clear();
    session::clear_session();
    // Close all saved failed fds so they are not re-enqueued on resume.
    let fds: Vec<RawFd> = crate::android::FAILED_FDS.lock().unwrap().drain().map(|(_, fd)| fd).collect();
    for fd in fds {
        unsafe { libc::close(fd) };
    }
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativePauseAll(
    _env: JNIEnv,
    _class: JClass,
) {
    PAUSE_FLAG.store(true, Ordering::Relaxed);
    runtime::pause_all(androidProgress::progress_manager());
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeResumeAll(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    // Collect paused keys before clearing state so we can remove their ProgressManager
    // entries after resume. This matches iOS behaviour: old worker threads that are still
    // running after a rapid pause→resume cannot add stale in-flight bytes to the new
    // run's tracking because the entry no longer exists. init_progress() will create
    // a fresh entry when the new run starts.
    let paused_keys: Vec<String> = {
        let sess = session::session();
        sess.files
            .values()
            .filter(|e| e.state == UploadState::Paused || e.state == UploadState::Failed)
            .map(|e| e.file_key.clone())
            .collect()
    };
    PAUSE_FLAG.store(false, Ordering::Relaxed);
    match runtime::resume_all(androidProgress::progress_manager()) {
        Ok(()) => {
            for key in &paused_keys {
                androidProgress::progress_manager().remove(key);
            }
            // Re-enqueue any files that previously failed so they can retry.
            let failed_fds: Vec<(String, RawFd)> = {
                let mut map = crate::android::FAILED_FDS.lock().unwrap();
                map.drain().collect()
            };
            for (key, fd) in failed_fds {
                enqueue_front(key, fd);
            }
            QUEUE_SIGNAL.notify_one();
            std::ptr::null_mut()
        }
        Err(msg) => env
            .new_string(&msg)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut()),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetProgressJson(
    mut env: JNIEnv,
    _class: JClass,
    transfer_id: JString,
    file_hash: JString,
) -> jstring {
    let tid: Option<String> = env
        .get_string(&transfer_id)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());
    let fh: Option<String> = env
        .get_string(&file_hash)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());
    let progress = session::session().get_progress(tid.as_deref(), fh.as_deref());
    let json: Vec<_> = progress.iter().map(|p| p.to_json()).collect();
    let s = serde_json::to_string(&json).unwrap_or_else(|_| "[]".to_string());
    env.new_string(&s)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetLiveProgressJson(
    mut env: JNIEnv,
    _class: JClass,
    transfer_id: JString,
    file_hash: JString,
) -> jstring {
    let tid: Option<String> = env
        .get_string(&transfer_id)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());
    let fh: Option<String> = env
        .get_string(&file_hash)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());

    let live = androidProgress::progress_manager().get_live_progress(tid.as_deref(), fh.as_deref());
    let live_keys: std::collections::HashSet<String> =
        live.iter().filter_map(|p| p.file_key.clone()).collect();
    let session_entries = session::session().get_progress(tid.as_deref(), fh.as_deref());
    let mut merged: Vec<_> = live;
    for p in session_entries {
        if p.file_key.as_ref().map_or(true, |k| !live_keys.contains(k)) {
            merged.push(p);
        }
    }

    let json: Vec<_> = merged.iter().map(|p| p.to_json()).collect();
    let s = serde_json::to_string(&json).unwrap_or_else(|_| "[]".to_string());
    env.new_string(&s)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetLiveAggregateProgressJson(
    mut env: JNIEnv,
    _class: JClass,
    transfer_id: JString,
) -> jstring {
    let tid: Option<String> = env
        .get_string(&transfer_id)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());
    let (session_agg, transfer_agg, current_tid) = {
        let sess = session::session();
        let s = sess.get_aggregate_progress(None);
        let t = sess.get_aggregate_progress(tid.as_deref());
        let ctid = tid
            .clone()
            .unwrap_or_else(|| sess.current_transfer_id.clone().unwrap_or_default());
        (s, t, ctid)
    };
    let (live_session, live_transfer) = androidProgress::progress_manager().get_live_aggregate(
        session_agg,
        transfer_agg,
        &current_tid,
    );
    let mut result = if tid.is_some() {
        live_transfer
    } else {
        live_session
    };

    // After all files complete, upload_orchestrator calls clear_session() which resets
    // global_state to NOT_STARTED before the next poll runs. The ProgressManager is not
    // cleared, so its entries still carry status=Completed. Use that to surface COMPLETED
    // to the polling service so it can terminate instead of looping forever.
    if result.state == GlobalUploaderState::NotStarted {
        let live = androidProgress::progress_manager().get_live_progress(tid.as_deref(), None);
        if !live.is_empty() && live.iter().all(|p| p.state == UploadState::Completed) {
            result.state = GlobalUploaderState::Completed;
        }
    }

    let s = serde_json::to_string(&result.to_json()).unwrap_or_else(|_| "{}".to_string());
    env.new_string(&s)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetGlobalState(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let state = session::session().global_state.as_str().to_string();
    env.new_string(&state)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetAggregateProgressJson(
    mut env: JNIEnv,
    _class: JClass,
    transfer_id: JString,
) -> jstring {
    let tid: Option<String> = env
        .get_string(&transfer_id)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());
    let agg = session::session().get_aggregate_progress(tid.as_deref());
    let s = serde_json::to_string(&agg.to_json()).unwrap_or_else(|_| "{}".to_string());
    env.new_string(&s)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
}
