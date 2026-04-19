pub mod progress;
pub mod api;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use futures::stream::{FuturesUnordered, StreamExt};
use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_file_reader::WebSysFile;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_futures::spawn_local;

use crate::core::{ChunkUploadResult, MAX_CONCURRENT_UPLOADS, MAX_RETRIES, clean_etag};
use crate::core::chunk::ChunkInfo;
use crate::core::config;
use crate::core::hash::sha256_web_file;
use crate::core::retry::{self, RetryPolicy};
use crate::core::session::{self, GlobalUploaderState, UploadState};
use crate::wasm::progress as wasmProgress;

// ---------------------------------------------------------------------------
// Sequential file upload queue (WASM is single-threaded — RefCell is safe)
// ---------------------------------------------------------------------------

struct FileQueueEntry {
    file_key: String,
    file: web_sys::File,
}

thread_local! {
    static FILE_QUEUE: RefCell<VecDeque<FileQueueEntry>> = RefCell::new(VecDeque::new());
    static QUEUE_RUNNING: RefCell<bool> = RefCell::new(false);
    /// Set to true by wasm_pause_all(); cleared when the running upload stops.
    static PAUSE_REQUESTED: RefCell<bool> = RefCell::new(false);
}

/// Enqueue a file for sequential upload. If no upload is currently running, starts immediately.
fn enqueue_file(file_key: String, file: web_sys::File) {
    FILE_QUEUE.with(|q| q.borrow_mut().push_back(FileQueueEntry { file_key, file }));
    maybe_start_next();
}

fn is_pause_requested() -> bool {
    PAUSE_REQUESTED.with(|p| *p.borrow())
}

/// Sentinel error type to distinguish a deliberate pause from a real failure.
#[derive(Debug)]
enum UploadOutcome {
    Completed,
    Paused,
    Failed(String),
}

/// If the queue is idle, pop the next entry and start it.
fn maybe_start_next() {
    // Don't start while pause is still in effect — the Paused outcome handler
    // clears this flag and calls maybe_start_next() itself once the XHR stops.
    if is_pause_requested() {
        return;
    }
    let already_running = QUEUE_RUNNING.with(|r| *r.borrow());
    if already_running {
        return;
    }
    let next = FILE_QUEUE.with(|q| q.borrow_mut().pop_front());
    if let Some(entry) = next {
        QUEUE_RUNNING.with(|r| *r.borrow_mut() = true);
        spawn_local(async move {
            let transfer_id = session::session()
                .files
                .get(&entry.file_key)
                .map(|e| e.transfer_id.clone())
                .unwrap_or_default();

            session::session().mark_file_state(&entry.file_key, UploadState::Running);
            let outcome = run_upload(&entry.file_key, &transfer_id, entry.file).await;
            match outcome {
                UploadOutcome::Completed => {
                    session::session().mark_file_state(&entry.file_key, UploadState::Completed);
                    let (s_agg, t_agg) = {
                        let sess = session::session();
                        (sess.get_aggregate_progress(None), sess.get_aggregate_progress(Some(&transfer_id)))
                    };
                    wasmProgress::progress_manager().set_status(&entry.file_key, UploadState::Completed, s_agg, t_agg);
                    // Auto-reset once every file in the session is done
                    let all_done = session::session().global_state == GlobalUploaderState::Completed;
                    if all_done {
                        session::clear_session();
                        wasmProgress::progress_manager().clear();
                    } else {
                        session::persist_session();
                    }
                }
                UploadOutcome::Paused => {
                    session::session().mark_file_state(&entry.file_key, UploadState::Paused);
                    session::persist_session();
                    // wasm_pause_all() already fired the Paused progress event proactively.
                    // Firing a second one here would overwrite any Running event sent by a
                    // concurrent resume, causing the UI to snap back to PAUSED.
                    PAUSE_REQUESTED.with(|p| *p.borrow_mut() = false);
                }
                UploadOutcome::Failed(e) => {
                    web_sys::console::error_1(
                        &format!("Upload failed for {}: {}", entry.file_key, e).into(),
                    );
                    session::session().mark_file_state(&entry.file_key, UploadState::Failed);
                    session::persist_session();
                    let (s_agg, t_agg) = {
                        let sess = session::session();
                        (sess.get_aggregate_progress(None), sess.get_aggregate_progress(Some(&transfer_id)))
                    };
                    wasmProgress::progress_manager().set_status(&entry.file_key, UploadState::Failed, s_agg, t_agg);
                }
            }

            QUEUE_RUNNING.with(|r| *r.borrow_mut() = false);
            maybe_start_next();
        });
    }
}

// ---------------------------------------------------------------------------
// Public wasm_bindgen exports
// ---------------------------------------------------------------------------

#[wasm_bindgen(start)]
pub fn wasm_start() {
    console_error_panic_hook::set_once();
}

/// Load persisted session from IndexedDB and restore it into the in-memory session.
/// Called once during worker init before any uploads are processed.
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
    config::set_config(&start_upload_api, &get_upload_urls_api, &complete_api);
}

/// Phase 1: compute hash, dedup-check, call startUploadApi, register in session.
/// Returns the S3 fileKey quickly — the actual chunk upload has NOT started yet.
/// Call wasm_run_file afterwards to start the upload in the background.
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

/// Phase 2: enqueue chunk uploads for sequential processing (fire-and-forget).
/// Must be called after wasm_start_file for files that are not yet completed.
#[wasm_bindgen]
pub fn wasm_run_file(file_key: String, file: web_sys::File) {
    // Skip if already completed
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

/// Upload a file. Returns the S3 fileKey on success.
/// Kept for backwards compatibility — prefers wasm_start_file + wasm_run_file for new callers.
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
    // Remove from pending queue if not yet started
    FILE_QUEUE.with(|q| q.borrow_mut().retain(|e| e.file_key != file_key));
    session::session().cancel_file(&file_key);
}

#[wasm_bindgen]
pub fn wasm_cancel_transfer(transfer_id: String) {
    FILE_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let sess = session::session();
        q.retain(|e| {
            sess.files.get(&e.file_key).map_or(true, |f| f.transfer_id != transfer_id)
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
    // Drain pending queue — running upload will stop after current chunk
    FILE_QUEUE.with(|q| q.borrow_mut().clear());
    PAUSE_REQUESTED.with(|p| *p.borrow_mut() = true);
    session::session().pause_all();

    // Fire Paused progress events for all currently tracked files
    let keys = wasmProgress::progress_manager().tracked_file_keys();
    for file_key in &keys {
        let (s_agg, t_agg) = {
            let sess = session::session();
            let tid = sess.files.get(file_key).map(|e| e.transfer_id.clone()).unwrap_or_default();
            (sess.get_aggregate_progress(None), sess.get_aggregate_progress(Some(&tid)))
        };
        wasmProgress::progress_manager().set_status(file_key, UploadState::Paused, s_agg, t_agg);
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

// ---------------------------------------------------------------------------
// Internal upload logic
// ---------------------------------------------------------------------------

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

/// Phase 1 logic: hash + dedup-check + startUploadApi + session registration.
/// Returns the fileKey. Does NOT start chunk uploads.
async fn start_file_internal(
    file: &web_sys::File,
    transfer_id: &str,
    user_params: &HashMap<String, String>,
) -> Result<String, String> {
    let file_hash = sha256_web_file(file, transfer_id).await?;

    {
        let sess = session::session();
        if let Some(entry) = sess.find_by_hash(&file_hash) {
            if entry.state == UploadState::Completed || entry.is_resumable() {
                return Ok(entry.file_key.clone());
            }
        }
    }

    let file_size = file.size() as u64;
    let file_name = file.name();

    let file_key = {
        let sess = session::session();
        let existing_key = sess.find_by_hash(&file_hash).map(|e| e.file_key.clone());
        drop(sess);

        if let Some(key) = existing_key {
            key
        } else {
            let start_resp = api::start_upload(&file_name, &file_hash, file_size, user_params).await?;
            let key = start_resp.key.clone();
            session::session().register_file(
                key.clone(),
                file_hash,
                transfer_id.to_string(),
                String::new(),
                file_name,
                file_size,
                user_params.clone(),
            );
            session::session().set_upload_info(
                &key,
                start_resp.upload_id,
                start_resp.part_size,
                ((file_size + start_resp.part_size - 1) / start_resp.part_size) as u32,
            );
            key
        }
    };

    Ok(file_key)
}

async fn upload_file_internal(
    file: web_sys::File,
    transfer_id: String,
    user_params: HashMap<String, String>,
) -> Result<String, String> {
    let file_key = start_file_internal(&file, &transfer_id, &user_params).await?;

    // Skip if already completed (dedup hit)
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

async fn run_upload(
    file_key: &str,
    transfer_id: &str,
    file: web_sys::File,
) -> UploadOutcome {
    let (upload_id, part_size, completed_etags) = {
        let sess = session::session();
        let entry = match sess.files.get(file_key) {
            Some(e) => e,
            None => return UploadOutcome::Failed("File entry not found".into()),
        };
        let upload_id = match entry.upload_id.clone() {
            Some(id) => id,
            None => return UploadOutcome::Failed("Missing upload_id".into()),
        };
        let part_size = match entry.part_size {
            Some(ps) => ps,
            None => return UploadOutcome::Failed("Missing part_size".into()),
        };
        (upload_id, part_size, entry.completed_chunk_etags.clone())
    };

    let file_size = file.size() as u64;
    let total_parts = ((file_size + part_size - 1) / part_size) as u32;

    let done_parts: std::collections::HashSet<u32> =
        completed_etags.iter().map(|(p, _)| *p).collect();
    let remaining_parts: Vec<u32> = (1..=total_parts)
        .filter(|p| !done_parts.contains(p))
        .collect();

    if remaining_parts.is_empty() {
        let all_results = completed_etags
            .into_iter()
            .map(|(pn, etag)| ChunkUploadResult { part_number: pn, etag })
            .collect();
        return match api::complete_upload(file_key, &upload_id, all_results).await {
            Ok(_) => UploadOutcome::Completed,
            Err(e) => UploadOutcome::Failed(e),
        };
    }

    // Init progress
    {
        let already_completed_bytes: u64 = completed_etags.iter()
            .map(|(pn, _)| {
                let start = (*pn as u64 - 1) * part_size;
                (file_size - start).min(part_size)
            })
            .sum();
        let already_completed_parts = completed_etags.len() as u32;
        let (s_agg, t_agg) = {
            let sess = session::session();
            let s = sess.get_aggregate_progress(None);
            let t = sess.get_aggregate_progress(Some(transfer_id));
            (s, t)
        };
        wasmProgress::progress_manager().init(
            file_key.to_string(),
            transfer_id.to_string(),
            file_size,
            total_parts,
            already_completed_bytes,
            already_completed_parts,
            s_agg,
            t_agg,
        );
    }

    // Prefetch first URL batch
    let first_batch: Vec<u32> = remaining_parts.iter().take(MAX_CONCURRENT_UPLOADS).cloned().collect();
    let mut url_pool: HashMap<u32, String> =
        match api::fetch_upload_urls_batch(file_key, &upload_id, &first_batch).await {
            Ok(urls) => urls,
            Err(e) => return UploadOutcome::Failed(e),
        };

    let file_arc = std::sync::Arc::new(file);
    let mut in_flight: FuturesUnordered<_> = FuturesUnordered::new();
    let mut next_idx = 0;
    let parts_len = remaining_parts.len();
    let completed_parts = std::sync::Arc::new(std::sync::Mutex::new(
        completed_etags.into_iter().map(|(pn, etag)| ChunkUploadResult { part_number: pn, etag }).collect::<Vec<_>>()
    ));

    // Helper: push a chunk task if its URL is available, otherwise add URL to a retry list
    let push_chunk = |idx: usize,
                          url_pool: &mut HashMap<u32, String>,
                          file_arc: &std::sync::Arc<web_sys::File>,
                          completed_parts: &std::sync::Arc<std::sync::Mutex<Vec<ChunkUploadResult>>>,
                          in_flight: &mut FuturesUnordered<_>| {
        let part_number = remaining_parts[idx];
        if let Some(url) = url_pool.remove(&part_number) {
            in_flight.push(upload_chunk_task(
                file_arc.clone(), part_number, part_size, file_size,
                url, file_key.to_string(), upload_id.to_string(), completed_parts.clone(),
            ));
            true
        } else {
            false
        }
    };

    // Seed the in-flight queue — skip parts whose URL is missing (shouldn't happen but be safe)
    while next_idx < parts_len && in_flight.len() < MAX_CONCURRENT_UPLOADS {
        push_chunk(next_idx, &mut url_pool, &file_arc, &completed_parts, &mut in_flight);
        next_idx += 1;
    }

    while let Some(result) = in_flight.next().await {
        // An Err here means either a real failure or an XHR abort triggered by pause
        if let Err(e) = result {
            if is_pause_requested() {
                // Remaining in-flight XHRs have already been aborted — just drain the futures
                while in_flight.next().await.is_some() {}
                return UploadOutcome::Paused;
            }
            while in_flight.next().await.is_some() {}
            return UploadOutcome::Failed(e);
        }

        // Check pause signal even on success (abort happened after onload in a race)
        if is_pause_requested() {
            while in_flight.next().await.is_some() {}
            return UploadOutcome::Paused;
        }

        // Enqueue next part
        if next_idx < parts_len {
            // Prefetch the next batch of URLs when the pool runs low
            if url_pool.len() < MAX_CONCURRENT_UPLOADS {
                let prefetch_parts: Vec<u32> = remaining_parts
                    .iter()
                    .skip(next_idx)
                    .take(MAX_CONCURRENT_UPLOADS)
                    .cloned()
                    .collect();
                if !prefetch_parts.is_empty() {
                    let new_urls = api::fetch_upload_urls_batch(file_key, &upload_id, &prefetch_parts)
                        .await
                        .unwrap_or_default();
                    url_pool.extend(new_urls);
                }
            }

            let pushed = push_chunk(next_idx, &mut url_pool, &file_arc, &completed_parts, &mut in_flight);
            let _ = pushed;
            next_idx += 1;
        }
    }

    let all_results = completed_parts.lock().unwrap().clone();
    match api::complete_upload(file_key, &upload_id, all_results).await {
        Ok(_) => UploadOutcome::Completed,
        Err(e) => UploadOutcome::Failed(e),
    }
}

async fn upload_chunk_task(
    file: std::sync::Arc<web_sys::File>,
    part_number: u32,
    part_size: u64,
    file_size: u64,
    url: String,
    file_key: String,
    _upload_id: String,
    completed_parts: std::sync::Arc<std::sync::Mutex<Vec<ChunkUploadResult>>>,
) -> Result<(), String> {
    let start_pos = (part_number as u64 - 1) * part_size;
    let chunk_size = (file_size - start_pos).min(part_size);
    let chunk_info = ChunkInfo { part_number, start_pos, chunk_size, url: url.clone() };

    let chunk = read_chunk_from_web_file(&file, &chunk_info)?;

    let etag = upload_chunk_with_retry(&url, &chunk, part_number, &file_key).await?;

    session::session().complete_chunk(&file_key, part_number, etag.clone(), chunk_size);
    session::persist_session();
    let (s_agg, t_agg) = {
        let sess = session::session();
        let tid = sess.files.get(&file_key).map(|e| e.transfer_id.clone()).unwrap_or_default();
        let s = sess.get_aggregate_progress(None);
        let t = sess.get_aggregate_progress(Some(&tid));
        (s, t)
    };
    wasmProgress::progress_manager().complete_chunk(&file_key, part_number, chunk_size, s_agg, t_agg);

    completed_parts.lock().unwrap().push(ChunkUploadResult { part_number, etag });
    Ok(())
}

async fn upload_chunk_with_retry(
    url: &str,
    data: &[u8],
    part_number: u32,
    file_key: &str,
) -> Result<String, String> {
    let policy = RetryPolicy::new(MAX_RETRIES);
    let file_key = file_key.to_string();
    let url = url.to_string();
    let data = data.to_vec();

    retry::run_with_retry_string_async(
        &policy,
        |_attempt| {
            let url = url.clone();
            let data = data.clone();
            let fk = file_key.clone();
            async move {
                if is_pause_requested() {
                    return Err("paused".to_string());
                }
                let (s_agg, t_agg) = {
                    let sess = session::session();
                    let tid = sess.files.get(&fk).map(|e| e.transfer_id.clone()).unwrap_or_default();
                    let s = sess.get_aggregate_progress(None);
                    let t = sess.get_aggregate_progress(Some(&tid));
                    (s, t)
                };
                wasmProgress::progress_manager().update_in_flight(&fk, part_number, 0, s_agg, t_agg);
                upload_chunk_xhr(&url, &data, part_number, &fk).await
            }
        },
        |_attempt, err, delay_ms| {
            let paused = err == "paused" || is_pause_requested();
            async move {
                if !paused {
                    sleep(delay_ms).await;
                }
            }
        },
    )
    .await
}

async fn upload_chunk_xhr(
    url: &str,
    data: &[u8],
    part_number: u32,
    file_key: &str,
) -> Result<String, String> {
    use wasm_bindgen::closure::Closure;
    use web_sys::XmlHttpRequest;

    // Abort immediately if pause was already requested before this chunk starts
    if is_pause_requested() {
        return Err("paused".into());
    }

    let xhr = XmlHttpRequest::new().map_err(|e| format!("{:?}", e))?;
    xhr.open("PUT", url).map_err(|e| format!("{:?}", e))?;

    let fk = file_key.to_string();
    let promise = Promise::new(&mut |resolve, reject| {
        let resolve_clone = resolve.clone();
        let reject_clone = reject.clone();
        let xhr_clone = xhr.clone();
        let xhr_for_progress = xhr.clone();
        let fk_clone = fk.clone();
        let reject_for_abort = reject.clone();

        let onprogress = Closure::wrap(Box::new(move |event: web_sys::ProgressEvent| {
            // Abort the XHR immediately if pause is requested
            if is_pause_requested() {
                xhr_for_progress.abort();
                return;
            }
            if event.length_computable() {
                let loaded = event.loaded() as u64;
                let (s_agg, t_agg) = {
                    let sess = session::session();
                    let tid = sess.files.get(&fk_clone).map(|e| e.transfer_id.clone()).unwrap_or_default();
                    let s = sess.get_aggregate_progress(None);
                    let t = sess.get_aggregate_progress(Some(&tid));
                    (s, t)
                };
                wasmProgress::progress_manager().update_in_flight(&fk_clone, part_number, loaded, s_agg, t_agg);
            }
        }) as Box<dyn FnMut(_)>);

        let onload = Closure::wrap(Box::new(move || {
            let status = xhr_clone.status().unwrap_or(0);
            if status >= 200 && status < 300 {
                let etag = xhr_clone
                    .get_response_header("etag")
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                resolve_clone.call1(&JsValue::NULL, &JsValue::from_str(&etag)).ok();
            } else {
                reject_clone
                    .call1(&JsValue::NULL, &JsValue::from_str(&format!("HTTP {}", status)))
                    .ok();
            }
        }) as Box<dyn FnMut()>);

        let onerror = Closure::wrap(Box::new(move || {
            reject.call1(&JsValue::NULL, &JsValue::from_str("Network error")).ok();
        }) as Box<dyn FnMut()>);

        // onabort fires when xhr.abort() is called (e.g. from the progress handler on pause)
        let onabort = Closure::wrap(Box::new(move || {
            reject_for_abort.call1(&JsValue::NULL, &JsValue::from_str("paused")).ok();
        }) as Box<dyn FnMut()>);

        if let Ok(upload) = xhr.upload() {
            upload.set_onprogress(Some(onprogress.as_ref().unchecked_ref()));
        }
        xhr.set_onload(Some(onload.as_ref().unchecked_ref()));
        xhr.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        xhr.set_onabort(Some(onabort.as_ref().unchecked_ref()));
        onprogress.forget();
        onload.forget();
        onerror.forget();
        onabort.forget();
    });

    let uint8_array = js_sys::Uint8Array::new_with_length(data.len() as u32);
    uint8_array.copy_from(data);
    xhr.send_with_opt_buffer_source(Some(&uint8_array))
        .map_err(|e| format!("{:?}", e))?;

    let result = JsFuture::from(promise)
        .await
        .map_err(|e| format!("{:?}", e))?;

    Ok(clean_etag(&result.as_string().unwrap_or_default()))
}

fn read_chunk_from_web_file(
    file: &web_sys::File,
    chunk_info: &ChunkInfo,
) -> Result<Vec<u8>, String> {
    let mut wf = WebSysFile::new(file.clone());
    chunk_info.read(&mut wf).map_err(|e| format!("Read failed: {}", e))
}

async fn sleep(ms: u32) {
    let promise = Promise::new(&mut |resolve, _| {
        if let Ok(worker) = js_sys::global().dyn_into::<web_sys::WorkerGlobalScope>() {
            worker.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32).ok();
        } else if let Some(window) = web_sys::window() {
            window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32).ok();
        }
    });
    JsFuture::from(promise).await.ok();
}
