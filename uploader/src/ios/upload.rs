use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::core::api;
use crate::core::chunk::ChunkInfo;
use crate::core::runtime;
use crate::core::session::{self, UploadState};
use crate::core::upload;
use crate::core::{clean_etag, ChunkUploadResult, MAX_CONCURRENT_UPLOADS, MAX_RETRIES};
use crate::ios::progress::{self as iosProgress, ProgressReader};
use crate::ios::{enqueue_key, init_nyquest, PAUSE_FLAG};

pub(super) fn run_upload(file_key: &str) {
    init_nyquest();

    let file_path = {
        let sess = session::session();
        let entry = match sess.files.get(file_key) {
            Some(e) => e.clone(),
            None => return,
        };
        entry.file_path
    };

    runtime::mark_state_persist(file_key, UploadState::Running);
    let result = upload_file_internal(file_key, &file_path);

    match result {
        Ok(_) => {
            runtime::mark_state_persist(file_key, UploadState::Completed);
            runtime::set_status(
                iosProgress::progress_manager(),
                file_key,
                UploadState::Completed,
            );
            if let Some(next_key) = session::session().next_pending_file() {
                enqueue_key(next_key);
            }
        }
        Err(e) if e.to_string() == "Upload paused" => {
            runtime::mark_state_persist(file_key, UploadState::Paused);
        }
        Err(e) => {
            eprintln!("Upload failed for {}: {}", file_key, e);
            runtime::mark_state_persist(file_key, UploadState::Failed);
            runtime::set_status(
                iosProgress::progress_manager(),
                file_key,
                UploadState::Failed,
            );
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

    if prepared.remaining_parts.is_empty() {
        return api::complete_upload(
            &client,
            file_key,
            &prepared.upload_id,
            upload::combine_upload_results(prepared.completed_etags, Vec::new()),
        );
    }

    runtime::init_progress(
        iosProgress::progress_manager(),
        file_key,
        file_size,
        prepared.total_parts,
        prepared.committed_bytes,
        prepared.done_parts.len() as u32,
    );

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
    let url_map = api::fetch_upload_urls_batch(client, file_key, upload_id, &parts_to_upload)?;
    let url_pool: Arc<Mutex<HashMap<u32, String>>> = Arc::new(Mutex::new(url_map));
    let parts_arc = Arc::new(parts_to_upload);

    let (tx, rx) = std::sync::mpsc::channel::<usize>();
    let rx = Arc::new(Mutex::new(rx));
    let abort = Arc::new(std::sync::atomic::AtomicBool::new(false));
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

                if PAUSE_FLAG.load(Ordering::Relaxed) {
                    abort.store(true, Ordering::Relaxed);
                    break;
                }

                let part_number = parts[part_idx];
                let url = match url_pool.lock().unwrap().get(&part_number).cloned() {
                    Some(u) => u,
                    None => {
                        eprintln!("No URL for part {}", part_number);
                        continue;
                    }
                };

                let chunk_info = ChunkInfo {
                    part_number,
                    start_pos: (part_number as u64 - 1) * part_size,
                    chunk_size: upload::part_size_for(part_number, part_size, file_size),
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
                        if !e.to_string().contains("paused")
                            && !e.to_string().contains("Interrupted")
                        {
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
    let policy = crate::core::retry::RetryPolicy::new(MAX_RETRIES);
    let file_key = file_key.to_string();

    crate::core::retry::run_with_retry_string(
        &policy,
        |_attempt| {
            runtime::update_in_flight(iosProgress::progress_manager(), &file_key, part_number, 0);

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
                        runtime::complete_chunk(
                            iosProgress::progress_manager(),
                            &file_key,
                            part_number,
                            etag.clone(),
                            chunk_size,
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
