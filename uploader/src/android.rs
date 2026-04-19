pub mod progress;

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Once};
use std::thread;

use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring};
use jni::JNIEnv;

use crate::android::progress::{self as androidProgress, ProgressReader};
use crate::core::api;
use crate::core::chunk::ChunkInfo;
use crate::core::config;
use crate::core::hash::sha256_fd;
use crate::core::retry::{self, RetryPolicy};
use crate::core::session::{self, UploadState};
use crate::core::upload::{self, StartDecision};
use crate::core::{clean_etag, ChunkUploadResult, MAX_CONCURRENT_UPLOADS, MAX_RETRIES};

fn init_logging() {
    let _ = android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            .with_tag("S3Uploader"),
    );
}

fn dup_fd(fd: RawFd) -> std::io::Result<RawFd> {
    let new_fd = unsafe { libc::dup(fd) };
    if new_fd == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(new_fd)
    }
}

fn build_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .connect_timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
}

// ---------------------------------------------------------------------------
// Upload queue — background worker thread
// ---------------------------------------------------------------------------

struct PendingUpload {
    file_key: String,
    raw_fd: RawFd,
}

static QUEUE: Mutex<std::collections::VecDeque<PendingUpload>> =
    Mutex::new(std::collections::VecDeque::new());
static QUEUE_SIGNAL: Condvar = Condvar::new();
static WORKER_STARTED: Once = Once::new();
pub(crate) static PAUSE_FLAG: AtomicBool = AtomicBool::new(false);

fn start_worker_thread() {
    WORKER_STARTED.call_once(|| {
        thread::spawn(|| loop {
            let pending = {
                let mut q = QUEUE.lock().unwrap();
                loop {
                    if let Some(p) = q.pop_front() {
                        break p;
                    }
                    q = QUEUE_SIGNAL.wait(q).unwrap();
                }
            };
            run_upload(&pending.file_key, pending.raw_fd);
        });
    });
}

fn enqueue(file_key: String, raw_fd: RawFd) {
    start_worker_thread();
    QUEUE
        .lock()
        .unwrap()
        .push_back(PendingUpload { file_key, raw_fd });
    QUEUE_SIGNAL.notify_one();
}

// ---------------------------------------------------------------------------
// Core upload logic
// ---------------------------------------------------------------------------

fn run_upload(file_key: &str, raw_fd: RawFd) {
    let transfer_id = {
        let sess = session::session();
        let entry = match sess.files.get(file_key) {
            Some(e) => e.clone(),
            None => {
                unsafe {
                    libc::close(raw_fd);
                }
                return;
            }
        };
        entry.transfer_id
    };

    session::session().mark_file_state(file_key, UploadState::Running);
    session::persist_session();

    let result = upload_file_internal(file_key, &transfer_id, raw_fd);

    unsafe {
        libc::close(raw_fd);
    }

    match result {
        Ok(_) => session::session().mark_file_state(file_key, UploadState::Completed),
        Err(e) => {
            if PAUSE_FLAG.load(Ordering::Relaxed) {
                log::info!("Upload paused for {}", file_key);
                session::session().mark_file_state(file_key, UploadState::Paused);
            } else {
                log::error!("Upload failed for {}: {}", file_key, e);
                session::session().mark_file_state(file_key, UploadState::Failed);
            }
        }
    }
    session::persist_session();

    if let Some(next_key) = session::session().next_pending_file() {
        // We don't have a new fd for queued-up files — they must call uploadFile again
        // unless the fd is stored. For now, signal failure so the caller re-provides the fd.
        log::debug!("Next pending file: {} (needs re-enqueue with fd)", next_key);
    }
}

fn upload_file_internal(
    file_key: &str,
    transfer_id: &str,
    raw_fd: RawFd,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = unsafe { File::from_raw_fd(dup_fd(raw_fd)?) };
    let file_size = file.metadata()?.len();
    let prepared = upload::prepare_upload(file_key, file_size)?;
    session::session().set_file_uploaded_bytes(file_key, prepared.committed_bytes);

    if prepared.remaining_parts.is_empty() {
        let client = build_client();
        return api::complete_upload_android(
            &client,
            file_key,
            &prepared.upload_id,
            upload::combine_upload_results(prepared.completed_etags, Vec::new()),
        );
    }

    {
        let (s_agg, t_agg) = {
            let sess = session::session();
            (
                sess.get_aggregate_progress(None),
                sess.get_aggregate_progress(Some(transfer_id)),
            )
        };
        androidProgress::progress_manager().init(
            file_key.to_string(),
            transfer_id.to_string(),
            file_size,
            prepared.total_parts,
            prepared.committed_bytes,
            prepared.done_parts.len() as u32,
            s_agg,
            t_agg,
        );
    }

    let client = build_client();
    let new_results = upload_parts_with_rolling_urls(
        &client,
        file_key,
        raw_fd,
        &prepared.upload_id,
        prepared.part_size,
        file_size,
        prepared.remaining_parts,
    )?;

    api::complete_upload_android(
        &client,
        file_key,
        &prepared.upload_id,
        upload::combine_upload_results(prepared.completed_etags, new_results),
    )
}

fn upload_parts_with_rolling_urls(
    client: &reqwest::blocking::Client,
    file_key: &str,
    raw_fd: RawFd,
    upload_id: &str,
    part_size: u64,
    file_size: u64,
    parts_to_upload: Vec<u32>,
) -> Result<Vec<ChunkUploadResult>, Box<dyn std::error::Error>> {
    let completed_parts = Arc::new(Mutex::new(Vec::<ChunkUploadResult>::new()));
    let url_pool: Arc<Mutex<HashMap<u32, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let parts_arc = Arc::new(parts_to_upload);
    let parts_len = parts_arc.len();

    let prefetch = |part_numbers: &[u32]| -> Result<(), Box<dyn std::error::Error>> {
        if part_numbers.is_empty() {
            return Ok(());
        }
        let batch =
            api::fetch_upload_urls_batch_android(client, file_key, upload_id, part_numbers)?;
        url_pool.lock().unwrap().extend(batch);
        Ok(())
    };

    prefetch(&parts_arc[..MAX_CONCURRENT_UPLOADS.min(parts_len)])?;

    let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(MAX_CONCURRENT_UPLOADS);
    let rx = Arc::new(Mutex::new(rx));
    let mut handles = vec![];

    for _ in 0..MAX_CONCURRENT_UPLOADS {
        let rx = rx.clone();
        let parts = parts_arc.clone();
        let url_pool = url_pool.clone();
        let completed = completed_parts.clone();
        let file_key_str = file_key.to_string();

        let handle = thread::spawn(move || {
            let client = build_client();
            loop {
                let part_idx = match rx.lock().unwrap().recv() {
                    Ok(i) => i,
                    Err(_) => break,
                };
                let part_number = parts[part_idx];
                let url = url_pool.lock().unwrap().get(&part_number).cloned();
                let url = match url {
                    Some(u) => u,
                    None => {
                        log::error!("No URL for part {}", part_number);
                        continue;
                    }
                };

                let chunk_info = ChunkInfo {
                    part_number,
                    start_pos: (part_number as u64 - 1) * part_size,
                    chunk_size: upload::part_size_for(part_number, part_size, file_size),
                    url: url.clone(),
                };

                let chunk = match read_chunk_from_fd(raw_fd, &chunk_info) {
                    Ok(d) => d,
                    Err(e) => {
                        log::error!("Failed to read chunk {}: {:?}", part_number, e);
                        continue;
                    }
                };

                let etag = match upload_chunk_with_retry(
                    &client,
                    &url,
                    &chunk,
                    part_number,
                    &file_key_str,
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        log::error!("Failed to upload part {}: {:?}", part_number, e);
                        continue;
                    }
                };

                completed
                    .lock()
                    .unwrap()
                    .push(ChunkUploadResult { part_number, etag });
            }
        });
        handles.push(handle);
    }

    for (idx, _) in parts_arc.iter().enumerate() {
        // Prefetch the CURRENT batch's URLs at the start of each new batch.
        // Initial prefetch covered indices 0..MAX_CONCURRENT; at idx=MAX_CONCURRENT we need
        // indices MAX_CONCURRENT..2*MAX_CONCURRENT, etc.
        if idx > 0 && idx % MAX_CONCURRENT_UPLOADS == 0 && idx < parts_len {
            let batch_end = (idx + MAX_CONCURRENT_UPLOADS).min(parts_len);
            let next: Vec<u32> = parts_arc[idx..batch_end].to_vec();
            let _ = prefetch(&next);
        }
        tx.send(idx).ok();
    }
    drop(tx);

    for handle in handles {
        handle.join().ok();
    }

    let results = completed_parts.lock().unwrap().clone();
    if results.len() != parts_arc.len() {
        return Err(format!("Only {}/{} parts uploaded", results.len(), parts_arc.len()).into());
    }
    Ok(results)
}

fn upload_chunk_with_retry(
    client: &reqwest::blocking::Client,
    url: &str,
    data: &[u8],
    part_number: u32,
    file_key: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let chunk_size = data.len() as u64;
    let policy = RetryPolicy::new(MAX_RETRIES);
    let file_key = file_key.to_string();

    retry::run_with_retry_string(
        &policy,
        |_attempt| {
            let (s_agg, t_agg) = {
                let sess = session::session();
                let tid = sess
                    .files
                    .get(&file_key)
                    .map(|e| e.transfer_id.clone())
                    .unwrap_or_default();
                let s = sess.get_aggregate_progress(None);
                let t = sess.get_aggregate_progress(Some(&tid));
                (s, t)
            };
            androidProgress::progress_manager().update_in_flight(
                &file_key,
                part_number,
                0,
                s_agg,
                t_agg,
            );

            let progress_reader = ProgressReader::new(data.to_vec(), file_key.clone(), part_number);
            let response = client
                .put(url)
                .header("Content-Length", chunk_size.to_string())
                .body(reqwest::blocking::Body::new(progress_reader))
                .send()
                .map_err(|e| format!("{:?}", e))?;

            let etag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(clean_etag)
                .ok_or_else(|| "No ETag in response".to_string())?;

            let (s_agg, t_agg) = {
                let sess = session::session();
                let tid = sess
                    .files
                    .get(&file_key)
                    .map(|e| e.transfer_id.clone())
                    .unwrap_or_default();
                let s = sess.get_aggregate_progress(None);
                let t = sess.get_aggregate_progress(Some(&tid));
                (s, t)
            };
            androidProgress::progress_manager().complete_chunk(
                &file_key,
                part_number,
                chunk_size,
                s_agg,
                t_agg,
            );
            session::session().complete_chunk(&file_key, part_number, etag.clone(), chunk_size);
            session::persist_session();
            Ok(etag)
        },
        |attempt, err, delay_ms| {
            log::warn!(
                "Upload attempt {} failed for part {}: {}, retrying in {}ms",
                attempt,
                part_number,
                err,
                delay_ms
            );
            thread::sleep(std::time::Duration::from_millis(delay_ms as u64));
        },
    )
    .map_err(|e| e.into())
}

fn read_chunk_from_fd(raw_fd: RawFd, chunk: &ChunkInfo) -> std::io::Result<Vec<u8>> {
    let file = unsafe { File::from_raw_fd(dup_fd(raw_fd)?) };
    let mut reader = BufReader::new(file);
    chunk.read(&mut reader)
}

// ---------------------------------------------------------------------------
// JNI entry points
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeSetConfig(
    mut env: JNIEnv,
    _class: JClass,
    start_upload_api: JString,
    get_upload_urls_api: JString,
    complete_api: JString,
) {
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
    config::set_config(&s, &g, &c);
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeSetStoragePath(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
) {
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

/// Returns the S3 fileKey as a JString (empty string on failure).
#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeUploadFile(
    mut env: JNIEnv,
    _class: JClass,
    fd: jint,
    transfer_id: JString,
    user_params_json: JString,
) -> jstring {
    init_logging();

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

    let raw_fd = fd as RawFd;

    // Compute hash and call startUploadApi
    let file_hash = match sha256_fd(raw_fd, &tid) {
        Ok(h) => h,
        Err(e) => {
            log::error!("sha256_fd failed: {}", e);
            return empty_jstring(&mut env);
        }
    };

    let file_key = match upload::start_decision(&file_hash) {
        StartDecision::Completed { file_key } => {
            return env
                .new_string(&file_key)
                .map(|s| s.into_raw())
                .unwrap_or(empty_jstring(&mut env));
        }
        StartDecision::Resume { file_key } => {
            let dup = match dup_fd(raw_fd) {
                Ok(f) => f,
                Err(_) => return empty_jstring(&mut env),
            };
            enqueue(file_key.clone(), dup);
            return env
                .new_string(&file_key)
                .map(|s| s.into_raw())
                .unwrap_or(empty_jstring(&mut env));
        }
        StartDecision::StartNew => {
            let dup_for_size = match dup_fd(raw_fd) {
                Ok(f) => f,
                Err(_) => return empty_jstring(&mut env),
            };
            let file_size = unsafe {
                let f = File::from_raw_fd(dup_for_size);
                f.metadata().map(|m| m.len()).unwrap_or(0)
            };

            let client = build_client();
            let start_resp = match api::start_upload_android(
                &client,
                "file",
                &file_hash,
                file_size,
                &user_params,
            ) {
                Ok(r) => r,
                Err(e) => {
                    log::error!("startUploadApi failed: {}", e);
                    return empty_jstring(&mut env);
                }
            };

            let file_key = upload::register_started_upload(
                file_hash,
                &tid,
                String::new(),
                "file".to_string(),
                file_size,
                user_params,
                start_resp,
            );
            session::persist_session();
            file_key
        }
    };

    let dup = match dup_fd(raw_fd) {
        Ok(f) => f,
        Err(_) => return empty_jstring(&mut env),
    };
    enqueue(file_key.clone(), dup);

    env.new_string(&file_key)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetFormattedTitle(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let s = session::session().format_title();
    env.new_string(&s)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeGetFormattedSubtitle(
    mut env: JNIEnv,
    _class: JClass,
) -> jstring {
    let s = session::session().format_subtitle();
    env.new_string(&s)
        .map(|s| s.into_raw())
        .unwrap_or(empty_jstring(&mut env))
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
    session::session().cancel_file(&k);
    session::persist_session();
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
    session::session().cancel_transfer(&t);
    session::persist_session();
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
    session::session().pause_all();
    session::persist_session();
}

#[no_mangle]
pub extern "system" fn Java_com_s3bguploader_HybridS3BgUploader_nativeResumeAll(
    _env: JNIEnv,
    _class: JClass,
) {
    PAUSE_FLAG.store(false, Ordering::Relaxed);
    let file_keys: Vec<String> = {
        let sess = session::session();
        sess.files
            .values()
            .filter(|e| e.state == UploadState::Paused || e.state == UploadState::Failed)
            .map(|e| e.file_key.clone())
            .collect()
    };
    for key in &file_keys {
        session::session().mark_file_state(key, UploadState::NotStarted);
    }
    session::persist_session();
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

/// Returns live per-file progress JSON, merging in-flight bytes from ProgressManager.
/// Files no longer in ProgressManager (completed/not-started) fall back to session data.
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

    // Live entries from ProgressManager (in-flight tracking).
    let live = androidProgress::progress_manager().get_live_progress(tid.as_deref(), fk.as_deref());
    let live_keys: std::collections::HashSet<String> =
        live.iter().map(|p| p.file_key.clone()).collect();

    // Session entries for files not currently tracked by ProgressManager.
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

/// Returns live aggregate progress JSON, merging in-flight bytes from ProgressManager on top of session baseline.
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
    // Return the aggregate scoped to the requested transfer (or session if none).
    let result = if tid.is_some() {
        live_transfer
    } else {
        live_session
    };
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
