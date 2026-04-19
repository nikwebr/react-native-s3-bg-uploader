use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::android::progress::{self as androidProgress, ProgressReader};
use crate::android::{build_client, dup_fd, PAUSE_FLAG};
use crate::core::api;
use crate::core::chunk::ChunkInfo;
use crate::core::runtime;
use crate::core::session::{self, UploadState};
use crate::core::upload;
use crate::core::{clean_etag, ChunkUploadResult, MAX_CONCURRENT_UPLOADS, MAX_RETRIES};

pub(super) fn run_upload(file_key: &str, raw_fd: RawFd) {
    if !session::session().files.contains_key(file_key) {
        unsafe { libc::close(raw_fd) };
        return;
    }

    runtime::mark_state_persist(file_key, UploadState::Running);
    let result = upload_file_internal(file_key, raw_fd);

    unsafe { libc::close(raw_fd) };

    match result {
        Ok(_) => session::session().mark_file_state(file_key, UploadState::Completed),
        Err(e) => {
            if PAUSE_FLAG.load(std::sync::atomic::Ordering::Relaxed) {
                log::info!("Upload paused for {}", file_key);
                session::session().mark_file_state(file_key, UploadState::Paused);
            } else {
                log::error!("Upload failed for {}: {}", file_key, e);
                session::session().mark_file_state(file_key, UploadState::Failed);
            }
        }
    }
    session::persist_session();
}

fn upload_file_internal(file_key: &str, raw_fd: RawFd) -> Result<(), Box<dyn std::error::Error>> {
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

    runtime::init_progress(
        androidProgress::progress_manager(),
        file_key,
        file_size,
        prepared.total_parts,
        prepared.committed_bytes,
        prepared.done_parts.len() as u32,
    );

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
                let url = match url_pool.lock().unwrap().get(&part_number).cloned() {
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
    let policy = crate::core::retry::RetryPolicy::new(MAX_RETRIES);
    let file_key = file_key.to_string();

    crate::core::retry::run_with_retry_string(
        &policy,
        |_attempt| {
            runtime::update_in_flight(
                androidProgress::progress_manager(),
                &file_key,
                part_number,
                0,
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

            runtime::complete_chunk(
                androidProgress::progress_manager(),
                &file_key,
                part_number,
                etag.clone(),
                chunk_size,
            );
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
