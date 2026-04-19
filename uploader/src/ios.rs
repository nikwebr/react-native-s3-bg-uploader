pub mod progress;

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::BufReader;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Once};
use std::thread;

use crate::core::api;
use crate::core::chunk::ChunkInfo;
use crate::core::config;
use crate::core::hash::sha256_file;
use crate::core::retry::{self, RetryPolicy};
use crate::core::session::{self, UploadState};
use crate::core::upload::{self, StartDecision};
use crate::core::{clean_etag, ChunkUploadResult, MAX_CONCURRENT_UPLOADS, MAX_RETRIES};
use crate::ios::progress::{self as iosProgress, ProgressReader};

static INIT: Once = Once::new();

fn init_nyquest() {
    INIT.call_once(|| {
        nyquest_backend_nsurlsession::register();
    });
}

// ---------------------------------------------------------------------------
// Upload queue — background worker thread
// ---------------------------------------------------------------------------

struct UploadQueue {
    pending: std::collections::VecDeque<String>,
}

static QUEUE: Mutex<UploadQueue> = Mutex::new(UploadQueue {
    pending: std::collections::VecDeque::new(),
});
static QUEUE_SIGNAL: Condvar = Condvar::new();
static WORKER_STARTED: Once = Once::new();
pub(crate) static PAUSE_FLAG: AtomicBool = AtomicBool::new(false);

fn start_worker_thread() {
    WORKER_STARTED.call_once(|| {
        thread::spawn(|| loop {
            let file_key = {
                let mut q = QUEUE.lock().unwrap();
                loop {
                    if let Some(k) = q.pending.pop_front() {
                        break k;
                    }
                    q = QUEUE_SIGNAL.wait(q).unwrap();
                }
            };
            run_upload(&file_key);
        });
    });
}

fn enqueue_key(file_key: String) {
    start_worker_thread();
    let mut q = QUEUE.lock().unwrap();
    q.pending.push_back(file_key);
    QUEUE_SIGNAL.notify_one();
}

// ---------------------------------------------------------------------------
// Core upload logic
// ---------------------------------------------------------------------------

fn run_upload(file_key: &str) {
    init_nyquest();

    let file_path = {
        let sess = session::session();
        let entry = match sess.files.get(file_key) {
            Some(e) => e.clone(),
            None => return,
        };
        entry.file_path
    };

    session::session().mark_file_state(file_key, UploadState::Running);
    session::persist_session();

    let result = upload_file_internal(file_key, &file_path);

    match result {
        Ok(_) => {
            session::session().mark_file_state(file_key, UploadState::Completed);
            session::persist_session();
            let (s_agg, t_agg) = {
                let sess = session::session();
                let tid = sess
                    .files
                    .get(file_key)
                    .map(|e| e.transfer_id.clone())
                    .unwrap_or_default();
                let s = sess.get_aggregate_progress(None);
                let t = sess.get_aggregate_progress(Some(&tid));
                (s, t)
            };
            iosProgress::progress_manager().set_status(
                file_key,
                UploadState::Completed,
                s_agg,
                t_agg,
            );
            if let Some(next_key) = session::session().next_pending_file() {
                enqueue_key(next_key);
            }
        }
        Err(e) if e.to_string() == "Upload paused" => {
            session::session().mark_file_state(file_key, UploadState::Paused);
            session::persist_session();
        }
        Err(e) => {
            eprintln!("Upload failed for {}: {}", file_key, e);
            session::session().mark_file_state(file_key, UploadState::Failed);
            session::persist_session();
            let (s_agg, t_agg) = {
                let sess = session::session();
                let tid = sess
                    .files
                    .get(file_key)
                    .map(|e| e.transfer_id.clone())
                    .unwrap_or_default();
                let s = sess.get_aggregate_progress(None);
                let t = sess.get_aggregate_progress(Some(&tid));
                (s, t)
            };
            iosProgress::progress_manager().set_status(file_key, UploadState::Failed, s_agg, t_agg);
            // Don't auto-retry — resume_all() picks up Failed files when the user resumes.
        }
    }
}

fn upload_file_internal(file_key: &str, file_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = nyquest::ClientBuilder::default()
        .request_timeout(std::time::Duration::from_secs(30))
        .build_blocking()
        .map_err(|e| format!("Failed to create client: {:?}", e))?;

    let file = File::open(file_path)?;
    let file_size = file.metadata()?.len();
    let prepared = upload::prepare_upload(file_key, file_size)?;
    session::session().set_file_uploaded_bytes(file_key, prepared.committed_bytes);
    let transfer_id = prepared.transfer_id.clone();

    if prepared.remaining_parts.is_empty() {
        return api::complete_upload(
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
                sess.get_aggregate_progress(Some(&transfer_id)),
            )
        };
        iosProgress::progress_manager().init(
            file_key.to_string(),
            transfer_id.clone(),
            file_size,
            prepared.total_parts,
            prepared.committed_bytes,
            prepared.done_parts.len() as u32,
            s_agg,
            t_agg,
        );
    }

    let new_results = upload_parts_with_rolling_urls(
        &client,
        file_key,
        file_path,
        &prepared.upload_id,
        prepared.part_size,
        file_size,
        prepared.remaining_parts,
    )?;

    api::complete_upload(
        &client,
        file_key,
        &prepared.upload_id,
        upload::combine_upload_results(prepared.completed_etags, new_results),
    )
}

fn upload_parts_with_rolling_urls(
    client: &nyquest::BlockingClient,
    file_key: &str,
    file_path: &str,
    upload_id: &str,
    part_size: u64,
    file_size: u64,
    parts_to_upload: Vec<u32>,
) -> Result<Vec<ChunkUploadResult>, Box<dyn std::error::Error>> {
    let completed_parts = Arc::new(Mutex::new(Vec::<ChunkUploadResult>::new()));

    // Fetch all presigned URLs upfront — avoids the sender loop blocking on
    // mid-upload API calls, which was causing workers to starve.
    let url_map = api::fetch_upload_urls_batch(client, file_key, upload_id, &parts_to_upload)?;
    let url_pool: Arc<Mutex<HashMap<u32, String>>> = Arc::new(Mutex::new(url_map));
    let parts_arc = Arc::new(parts_to_upload);

    let (tx, rx) = std::sync::mpsc::channel::<usize>();
    let rx = Arc::new(Mutex::new(rx));
    // Set when any worker aborts (pause OR error) — stops the sender loop early.
    let abort = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Set only when a non-pause upload error causes the abort.
    let error_abort = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut handles = vec![];
    for _ in 0..MAX_CONCURRENT_UPLOADS {
        let rx = rx.clone();
        let parts = parts_arc.clone();
        let url_pool = url_pool.clone();
        let completed = completed_parts.clone();
        let file_path = file_path.to_string();
        let file_key_str = file_key.to_string();
        let abort = abort.clone();
        let error_abort = error_abort.clone();

        let handle = thread::spawn(move || {
            let client = nyquest::ClientBuilder::default()
                .request_timeout(std::time::Duration::from_secs(300))
                .build_blocking()
                .expect("Failed to create client in worker thread");

            loop {
                let part_idx = match rx.lock().unwrap().recv() {
                    Ok(i) => i,
                    Err(_) => break,
                };

                // Check pause before starting this chunk
                if PAUSE_FLAG.load(Ordering::Relaxed) {
                    abort.store(true, Ordering::Relaxed);
                    break;
                }

                let part_number = parts[part_idx];
                let url = {
                    let pool = url_pool.lock().unwrap();
                    pool.get(&part_number).cloned()
                };

                let url = match url {
                    Some(u) => u,
                    None => {
                        eprintln!("No URL for part {}", part_number);
                        continue;
                    }
                };

                let chunk_info = ChunkInfo {
                    part_number,
                    start_pos: (part_number as u64 - 1) * part_size,
                    chunk_size: {
                        let start = (part_number as u64 - 1) * part_size;
                        let remaining = file_size.saturating_sub(start);
                        remaining.min(part_size)
                    },
                    url: url.clone(),
                };

                let chunk = match read_chunk_from_file(&file_path, &chunk_info) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("Failed to read chunk {}: {:?}", part_number, e);
                        abort.store(true, Ordering::Relaxed);
                        error_abort.store(true, Ordering::Relaxed);
                        break;
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
                        abort.store(true, Ordering::Relaxed);
                        if e.to_string().contains("paused") || e.to_string().contains("Interrupted")
                        {
                            // pause — don't set error_abort
                        } else {
                            eprintln!("Failed to upload part {}: {:?}", part_number, e);
                            error_abort.store(true, Ordering::Relaxed);
                        }
                        break;
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

    // Send all work indices — URL pool is fully populated, no blocking prefetch needed.
    for (idx, _) in parts_arc.iter().enumerate() {
        if PAUSE_FLAG.load(Ordering::Relaxed) || abort.load(Ordering::Relaxed) {
            break;
        }
        tx.send(idx).ok();
    }
    drop(tx);

    for handle in handles {
        handle.join().ok();
    }

    if PAUSE_FLAG.load(Ordering::Relaxed)
        || (abort.load(Ordering::Relaxed) && !error_abort.load(Ordering::Relaxed))
    {
        return Err("Upload paused".into());
    }

    let results = completed_parts.lock().unwrap().clone();
    if results.len() != parts_arc.len() {
        return Err(format!(
            "Only {}/{} parts uploaded successfully",
            results.len(),
            parts_arc.len()
        )
        .into());
    }
    Ok(results)
}

fn upload_chunk_with_retry(
    client: &nyquest::BlockingClient,
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
            iosProgress::progress_manager().update_in_flight(
                &file_key,
                part_number,
                0,
                s_agg,
                t_agg,
            );

            let progress_reader = ProgressReader::new(data.to_vec(), file_key.clone(), part_number);
            let body = nyquest::blocking::Body::stream(
                progress_reader,
                "application/octet-stream",
                chunk_size,
            );
            let request = nyquest::Request::put(url.to_string()).with_body(body);

            match client.request(request) {
                Ok(response) => match response.get_header("etag") {
                    Ok(etag_vec) if !etag_vec.is_empty() => {
                        let etag = clean_etag(&etag_vec[0]);
                        // Update session FIRST so aggregate bytes are correct
                        session::session().complete_chunk(
                            &file_key,
                            part_number,
                            etag.clone(),
                            chunk_size,
                        );
                        session::persist_session();
                        // Then fire progress notifier (subtitle reads updated session bytes)
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
                        iosProgress::progress_manager().complete_chunk(
                            &file_key,
                            part_number,
                            chunk_size,
                            s_agg,
                            t_agg,
                        );
                        Ok(etag)
                    }
                    _ => Err("No ETag in response".to_string()),
                },
                Err(e) => Err(format!("{:?}", e)),
            }
        },
        |attempt, err, delay_ms| {
            eprintln!(
                "Upload attempt {} failed for part {}: {}, retrying in {}ms",
                attempt, part_number, err, delay_ms
            );
            thread::sleep(std::time::Duration::from_millis(delay_ms as u64));
        },
    )
    .map_err(|e| e.into())
}

fn read_chunk_from_file(path: &str, chunk: &ChunkInfo) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    chunk.read(&mut reader)
}

// ---------------------------------------------------------------------------
// C FFI exports
// ---------------------------------------------------------------------------

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
    config::set_config(s, g, c);
}

#[no_mangle]
pub extern "C" fn set_storage_path(path: *const c_char) {
    let p = unsafe { CStr::from_ptr(path) }.to_str().unwrap_or("");
    session::set_storage_path(p);
}

/// Returns the S3 fileKey as a null-terminated C string (caller must free with free_string).
/// Returns null on failure.
#[no_mangle]
pub extern "C" fn upload_file(
    file_path: *const c_char,
    transfer_id: *const c_char,
    user_params_json: *const c_char, // JSON object string, may be null
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
    PAUSE_FLAG.store(true, Ordering::Relaxed);
    session::session().pause_all();
    session::persist_session();

    // Fire Paused progress events immediately (before in-flight chunks abort),
    // so the UI sees the state change without a progress regression — same as WASM.
    let keys = iosProgress::progress_manager().tracked_file_keys();
    for file_key in &keys {
        let (s_agg, t_agg) = {
            let sess = session::session();
            let tid = sess
                .files
                .get(file_key)
                .map(|e| e.transfer_id.clone())
                .unwrap_or_default();
            let s = sess.get_aggregate_progress(None);
            let t = sess.get_aggregate_progress(Some(&tid));
            (s, t)
        };
        iosProgress::progress_manager().set_status(file_key, UploadState::Paused, s_agg, t_agg);
    }
}

#[no_mangle]
pub extern "C" fn resume_all() {
    PAUSE_FLAG.store(false, Ordering::Relaxed);
    // Drain stale keys queued before/during pause to avoid double-uploads.
    QUEUE.lock().unwrap().pending.clear();
    if let Some(next) = session::session().next_pending_file() {
        enqueue_key(next);
    }
}

#[no_mangle]
pub extern "C" fn get_progress_json(
    transfer_id: *const c_char, // nullable
    file_key: *const c_char,    // nullable
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
pub extern "C" fn get_aggregate_progress_json(
    transfer_id: *const c_char, // nullable
) -> *mut c_char {
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

/// Returns the formatted title string (caller must free with free_string).
#[no_mangle]
pub extern "C" fn format_title_string() -> *mut c_char {
    let s = session::session().format_title();
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Returns the formatted subtitle string (caller must free with free_string).
#[no_mangle]
pub extern "C" fn format_subtitle_string() -> *mut c_char {
    let s = session::session().format_subtitle();
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

/// Free a C string previously returned by upload_file / get_progress_json etc.
#[no_mangle]
pub extern "C" fn free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe { drop(CString::from_raw(s)) };
    }
}
