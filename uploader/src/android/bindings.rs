use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::sync::atomic::Ordering;

use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring};
use jni::JNIEnv;

use crate::android::progress as androidProgress;
use crate::android::{PAUSE_FLAG, QUEUE_SIGNAL};
use crate::native::NativeSessionStore;
use crate::core::runtime;
use crate::core::session::{self, register_store, format_template, GlobalUploaderState, UploadState};

fn ensure_store() {
    register_store(NativeSessionStore);
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

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeUploadFile(
    mut env: JNIEnv,
    _class: JClass,
    fd: jint,
    transfer_id: JString,
    user_params_json: JString,
) -> jstring {
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

    match super::start_and_enqueue(fd as RawFd, &tid, user_params) {
        Ok(file_key) => env
            .new_string(&file_key)
            .map(|s| s.into_raw())
            .unwrap_or(empty_jstring(&mut env)),
        Err(e) => {
            log::error!("nativeUploadFile failed: {}", e);
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
    file_key: JString,
) {
    let k: String = env
        .get_string(&file_key)
        .map(|s| s.into())
        .unwrap_or_default();
    session::cancel_file(&k);
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
    session::cancel_transfer(&t);
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeCancelAll(
    _env: JNIEnv,
    _class: JClass,
) {
    session::clear_session();
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
    _env: JNIEnv,
    _class: JClass,
) {
    let resumed_keys: Vec<String> = {
        let sess = session::session();
        sess.files
            .values()
            .filter(|e| e.state == UploadState::Paused || e.state == UploadState::Failed)
            .map(|e| e.file_key.clone())
            .collect()
    };
    PAUSE_FLAG.store(false, Ordering::Relaxed);
    session::resume_all();
    for key in &resumed_keys {
        androidProgress::progress_manager().remove(key);
    }
    QUEUE_SIGNAL.notify_one();
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetProgressJson(
    mut env: JNIEnv,
    _class: JClass,
    transfer_id: JString,
    file_key: JString,
) -> jstring {
    let tid: Option<String> = env
        .get_string(&transfer_id)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());
    let fk: Option<String> = env
        .get_string(&file_key)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());
    let progress = session::session().get_progress(tid.as_deref(), fk.as_deref());
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
    file_key: JString,
) -> jstring {
    let tid: Option<String> = env
        .get_string(&transfer_id)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());
    let fk: Option<String> = env
        .get_string(&file_key)
        .map(|s| s.into())
        .ok()
        .filter(|s: &String| !s.is_empty());

    let live = androidProgress::progress_manager().get_live_progress(tid.as_deref(), fk.as_deref());
    let live_keys: std::collections::HashSet<String> =
        live.iter().map(|p| p.file_key.clone()).collect();
    let session_entries = session::session().get_progress(tid.as_deref(), fk.as_deref());
    let mut merged: Vec<_> = live;
    for p in session_entries {
        if !live_keys.contains(&p.file_key) {
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
