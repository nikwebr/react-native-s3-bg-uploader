pub mod progress;
pub mod api;

use wasm_bindgen::prelude::*;
use wasm_bindgen_file_reader::WebSysFile;
use wasm_bindgen_futures::JsFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use js_sys::{Promise, Uint8Array, Function};
use std::sync::{Arc, Mutex};
use crate::core::{
    ChunkUploadResult,
    MAX_CONCURRENT_UPLOADS, MAX_RETRIES,
    clean_etag
};
use crate::core::chunk::{self, ChunkInfo};
use crate::core::retry::{self, RetryPolicy};

#[wasm_bindgen]
pub fn add(one: f64, two: f64) -> f64 {
    one + two
}

#[wasm_bindgen]
pub async fn upload_file(file: web_sys::File) -> Result<JsValue, JsValue> {
    upload_file_internal(file)
        .await
        .map(|_| JsValue::from_str("Upload successful"))
        .map_err(|e| JsValue::from_str(&format!("Upload failed: {}", e)))
}

async fn upload_file_internal(file: web_sys::File) -> Result<(), String> {
    let file_size = file.size() as u64;

    // Upload URLs abrufen
    let upload_info = api::fetch_upload_urls(file_size).await?;

    // Progress initialisieren
    progress::progress_manager().init(file_size, upload_info.chunk_count());

    // Chunk-Infos generieren
    let chunk_infos = chunk::generate_chunk_infos(file_size, &upload_info)?;

    // Shared state
    let file_arc = Arc::new(file);
    let chunk_infos_arc = Arc::new(chunk_infos);

    // Chunks parallel hochladen mit Sliding Window
    let completed_parts = upload_chunks_parallel_async(file_arc, chunk_infos_arc).await?;

    // Upload abschließen
    api::complete_upload_async(&upload_info, completed_parts).await?;

    Ok(())
}

async fn upload_chunks_parallel_async(
    file: Arc<web_sys::File>,
    chunk_infos: Arc<Vec<ChunkInfo>>,
) -> Result<Vec<ChunkUploadResult>, String> {
    let completed_parts = Arc::new(Mutex::new(Vec::new()));

    // Sliding Window: Starte bis zu MAX_CONCURRENT_UPLOADS gleichzeitig,
    // und starte einen neuen sobald einer fertig ist
    let mut in_flight: FuturesUnordered<_> = FuturesUnordered::new();
    let mut next_chunk_idx: usize = 0;

    // Initiale Befüllung der Queue
    while next_chunk_idx < chunk_infos.len() && in_flight.len() < MAX_CONCURRENT_UPLOADS {
        let future = upload_chunk_task(
            file.clone(),
            chunk_infos.clone(),
            next_chunk_idx,
            completed_parts.clone(),
        );

        in_flight.push(future);
        next_chunk_idx += 1;
    }

    // Verarbeite fertige Uploads und starte neue
    while let Some(result) = in_flight.next().await {
        // Fehler prüfen
        if let Err(e) = result {
            return Err(format!("Chunk upload failed: {}", e));
        }

        // Neuen Chunk starten, falls noch welche übrig sind
        if next_chunk_idx < chunk_infos.len() {
            let future = upload_chunk_task(
                file.clone(),
                chunk_infos.clone(),
                next_chunk_idx,
                completed_parts.clone(),
            );

            in_flight.push(future);
            next_chunk_idx += 1;
        }
    }

    let parts = completed_parts.lock().unwrap().clone();
    Ok(parts)
}

async fn upload_chunk_task(
    file: Arc<web_sys::File>,
    chunk_infos: Arc<Vec<ChunkInfo>>,
    chunk_idx: usize,
    completed_parts: Arc<Mutex<Vec<ChunkUploadResult>>>,
) -> Result<(), String> {
    let chunk_info = &chunk_infos[chunk_idx];

    // Chunk aus Datei lesen
    let chunk = read_chunk_from_web_file(&file, chunk_info)?;

    // Upload mit Retry und Progress-Tracking
    let etag = upload_chunk_with_retry_and_progress(&chunk_info.url, &chunk, chunk_info.part_number).await?;

    // Completed part speichern
    {
        let mut parts = completed_parts.lock().unwrap();
        parts.push(ChunkUploadResult {
            part_number: chunk_info.part_number,
            etag
        });
    }

    // Chunk als abgeschlossen markieren
    progress::progress_manager().complete_chunk(chunk_info.part_number, chunk_info.chunk_size);

    Ok(())
}

async fn upload_chunk_with_retry_and_progress(
    url: &str,
    data: &[u8],
    part_number: u32,
) -> Result<String, String> {
    let policy = RetryPolicy::new(MAX_RETRIES);

    retry::run_with_retry_string_async(
        &policy,
        |_attempt| async {
            progress::progress_manager().update_in_flight(part_number, 0);
            upload_chunk_with_progress(url, data, part_number).await
        },
        |attempt, err, delay_ms| {
            let err = err.to_string();
            async move {
                log_retry_attempt(attempt, part_number, &err, delay_ms);
                sleep(delay_ms).await;
            }
        },
    )
        .await
}

// uses XMLHttpRequest for progress tracking of inflight parts
async fn upload_chunk_with_progress(
    url: &str,
    data: &[u8],
    part_number: u32,
) -> Result<String, String> {
    use wasm_bindgen::closure::Closure;
    use web_sys::XmlHttpRequest;

    let xhr = XmlHttpRequest::new()
        .map_err(|e| format!("Failed to create XMLHttpRequest: {:?}", e))?;

    xhr.open("PUT", url)
        .map_err(|e| format!("Failed to open request: {:?}", e))?;

    // Promise für async/await
    let promise = Promise::new(&mut |resolve, reject| {
        let resolve_clone = resolve.clone();
        let reject_clone = reject.clone();
        let xhr_clone = xhr.clone();

        // Progress Event Handler
        let pn = part_number;
        let onprogress = Closure::wrap(Box::new(move |event: web_sys::ProgressEvent| {
            if event.length_computable() {
                let loaded = event.loaded() as u64;
                progress::progress_manager().update_in_flight(pn, loaded);
            }
        }) as Box<dyn FnMut(_)>);

        // Load Event Handler (Erfolg)
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
                reject_clone.call1(&JsValue::NULL, &JsValue::from_str(&format!("HTTP error: {}", status))).ok();
            }
        }) as Box<dyn FnMut()>);

        // Error Event Handler
        let onerror = Closure::wrap(Box::new(move || {
            reject.call1(&JsValue::NULL, &JsValue::from_str("Network error")).ok();
        }) as Box<dyn FnMut()>);

        // Event Listener setzen
        if let Ok(upload) = xhr.upload() {
            upload.set_onprogress(Some(onprogress.as_ref().unchecked_ref()));
        }
        xhr.set_onload(Some(onload.as_ref().unchecked_ref()));
        xhr.set_onerror(Some(onerror.as_ref().unchecked_ref()));

        // Closures am Leben halten
        onprogress.forget();
        onload.forget();
        onerror.forget();
    });

    // Daten senden
    let uint8_array = Uint8Array::new_with_length(data.len() as u32);
    uint8_array.copy_from(data);

    xhr.send_with_opt_buffer_source(Some(&uint8_array))
        .map_err(|e| format!("Failed to send request: {:?}", e))?;

    // Auf Completion warten
    let result = JsFuture::from(promise)
        .await
        .map_err(|e| format!("Upload failed: {:?}", e))?;

    let etag = result.as_string().unwrap_or_default();
    Ok(clean_etag(&etag))
}

fn read_chunk_from_web_file(
    file: &web_sys::File,
    chunk_info: &ChunkInfo
) -> Result<Vec<u8>, String> {
    let mut wf = WebSysFile::new(file.clone());
    chunk_info.read(&mut wf)
        .map_err(|e| format!("Read failed: {}", e))
}

fn log_retry_attempt(attempt: usize, part_number: u32, err: &str, delay_ms: u32) {
    web_sys::console::log_1(&format!(
        "Upload attempt {} failed for part {}: {}, retrying in {}ms",
        attempt, part_number, err, delay_ms
    ).into());
}

async fn sleep(ms: u32) {
    let promise = Promise::new(&mut |resolve, _| {
        // Versuche zuerst WorkerGlobalScope (für Web Worker)
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
