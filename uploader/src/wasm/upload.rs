use std::collections::HashMap;

use futures::stream::{FuturesUnordered, StreamExt};
use js_sys::Promise;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_file_reader::WebSysFile;
use wasm_bindgen_futures::JsFuture;

use crate::core::chunk::ChunkInfo;
use crate::core::runtime;
use crate::core::session;
use crate::core::upload;
use crate::core::{clean_etag, ChunkUploadResult, MAX_CONCURRENT_UPLOADS, MAX_RETRIES};
use crate::wasm::api;
use crate::wasm::progress as wasmProgress;
use crate::wasm::{is_pause_requested, UploadOutcome};

pub(super) async fn run_upload(file_key: &str, file: web_sys::File) -> UploadOutcome {
    let file_size = file.size() as u64;
    let prepared = match upload::prepare_upload(file_key, file_size) {
        Ok(prepared) => prepared,
        Err(err) => return UploadOutcome::Failed(err),
    };
    session::session().set_file_uploaded_bytes(file_key, prepared.committed_bytes);

    if prepared.remaining_parts.is_empty() {
        return match api::complete_upload(
            file_key,
            &prepared.upload_id,
            upload::combine_upload_results(prepared.completed_etags, Vec::new()),
        )
        .await
        {
            Ok(_) => UploadOutcome::Completed,
            Err(e) => UploadOutcome::Failed(e),
        };
    }

    runtime::init_progress(
        wasmProgress::progress_manager(),
        file_key,
        file_size,
        prepared.total_parts,
        prepared.committed_bytes,
        prepared.done_parts.len() as u32,
    );

    let first_batch: Vec<u32> = prepared
        .remaining_parts
        .iter()
        .take(MAX_CONCURRENT_UPLOADS)
        .cloned()
        .collect();
    let mut url_pool: HashMap<u32, String> =
        match api::fetch_upload_urls_batch(file_key, &prepared.upload_id, &first_batch).await {
            Ok(urls) => urls,
            Err(e) => return UploadOutcome::Failed(e),
        };

    let file_arc = std::sync::Arc::new(file);
    let mut in_flight: FuturesUnordered<_> = FuturesUnordered::new();
    let mut next_idx = 0;
    let parts_len = prepared.remaining_parts.len();
    let completed_parts = std::sync::Arc::new(std::sync::Mutex::new(
        upload::combine_upload_results(prepared.completed_etags, Vec::new()),
    ));

    let push_chunk =
        |idx: usize,
         url_pool: &mut HashMap<u32, String>,
         file_arc: &std::sync::Arc<web_sys::File>,
         completed_parts: &std::sync::Arc<std::sync::Mutex<Vec<ChunkUploadResult>>>,
         in_flight: &mut FuturesUnordered<_>| {
            let part_number = prepared.remaining_parts[idx];
            if let Some(url) = url_pool.remove(&part_number) {
                in_flight.push(upload_chunk_task(
                    file_arc.clone(),
                    part_number,
                    prepared.part_size,
                    file_size,
                    url,
                    file_key.to_string(),
                    completed_parts.clone(),
                ));
                true
            } else {
                false
            }
        };

    while next_idx < parts_len && in_flight.len() < MAX_CONCURRENT_UPLOADS {
        push_chunk(
            next_idx,
            &mut url_pool,
            &file_arc,
            &completed_parts,
            &mut in_flight,
        );
        next_idx += 1;
    }

    while let Some(result) = in_flight.next().await {
        if let Err(e) = result {
            if is_pause_requested() {
                while in_flight.next().await.is_some() {}
                return UploadOutcome::Paused;
            }
            while in_flight.next().await.is_some() {}
            return UploadOutcome::Failed(e);
        }

        if is_pause_requested() {
            while in_flight.next().await.is_some() {}
            return UploadOutcome::Paused;
        }

        if next_idx < parts_len {
            if url_pool.len() < MAX_CONCURRENT_UPLOADS {
                let prefetch_parts: Vec<u32> = prepared
                    .remaining_parts
                    .iter()
                    .skip(next_idx)
                    .take(MAX_CONCURRENT_UPLOADS)
                    .cloned()
                    .collect();
                if !prefetch_parts.is_empty() {
                    let new_urls = api::fetch_upload_urls_batch(
                        file_key,
                        &prepared.upload_id,
                        &prefetch_parts,
                    )
                    .await
                    .unwrap_or_default();
                    url_pool.extend(new_urls);
                }
            }

            let _ = push_chunk(
                next_idx,
                &mut url_pool,
                &file_arc,
                &completed_parts,
                &mut in_flight,
            );
            next_idx += 1;
        }
    }

    let all_results = completed_parts.lock().unwrap().clone();
    match api::complete_upload(file_key, &prepared.upload_id, all_results).await {
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
    completed_parts: std::sync::Arc<std::sync::Mutex<Vec<ChunkUploadResult>>>,
) -> Result<(), String> {
    let start_pos = (part_number as u64 - 1) * part_size;
    let chunk_size = upload::part_size_for(part_number, part_size, file_size);
    let chunk_info = ChunkInfo {
        part_number,
        start_pos,
        chunk_size,
        url: url.clone(),
    };

    let chunk = read_chunk_from_web_file(&file, &chunk_info)?;
    let etag = upload_chunk_with_retry(&url, &chunk, part_number, &file_key).await?;

    runtime::complete_chunk(
        wasmProgress::progress_manager(),
        &file_key,
        part_number,
        etag.clone(),
        chunk_size,
    );
    completed_parts
        .lock()
        .unwrap()
        .push(ChunkUploadResult { part_number, etag });
    Ok(())
}

async fn upload_chunk_with_retry(
    url: &str,
    data: &[u8],
    part_number: u32,
    file_key: &str,
) -> Result<String, String> {
    let policy = crate::core::retry::RetryPolicy::new(MAX_RETRIES);
    let file_key = file_key.to_string();
    let url = url.to_string();
    let data = data.to_vec();

    crate::core::retry::run_with_retry_string_async(
        &policy,
        |_attempt| {
            let url = url.clone();
            let data = data.clone();
            let fk = file_key.clone();
            async move {
                if is_pause_requested() {
                    return Err("paused".to_string());
                }
                runtime::update_in_flight(wasmProgress::progress_manager(), &fk, part_number, 0);
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
            if is_pause_requested() {
                let _ = xhr_for_progress.abort();
                return;
            }
            if event.length_computable() {
                runtime::update_in_flight(
                    wasmProgress::progress_manager(),
                    &fk_clone,
                    part_number,
                    event.loaded() as u64,
                );
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
                resolve_clone
                    .call1(&JsValue::NULL, &JsValue::from_str(&etag))
                    .ok();
            } else {
                reject_clone
                    .call1(
                        &JsValue::NULL,
                        &JsValue::from_str(&format!("HTTP {}", status)),
                    )
                    .ok();
            }
        }) as Box<dyn FnMut()>);

        let onerror = Closure::wrap(Box::new(move || {
            reject
                .call1(&JsValue::NULL, &JsValue::from_str("Network error"))
                .ok();
        }) as Box<dyn FnMut()>);

        let onabort = Closure::wrap(Box::new(move || {
            reject_for_abort
                .call1(&JsValue::NULL, &JsValue::from_str("paused"))
                .ok();
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
    chunk_info
        .read(&mut wf)
        .map_err(|e| format!("Read failed: {}", e))
}

async fn sleep(ms: u32) {
    let promise = Promise::new(&mut |resolve, _| {
        if let Ok(worker) = js_sys::global().dyn_into::<web_sys::WorkerGlobalScope>() {
            worker
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
                .ok();
        } else if let Some(window) = web_sys::window() {
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32)
                .ok();
        }
    });
    JsFuture::from(promise).await.ok();
}
