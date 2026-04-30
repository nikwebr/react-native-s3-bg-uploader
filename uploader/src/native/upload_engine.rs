use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::core::chunk::ChunkInfo;
use crate::core::upload;
use crate::core::{ChunkUploadResult, MAX_CONCURRENT_UPLOADS};

#[derive(Clone, Copy)]
pub enum BlockingPrefetch {
    AllUpfront,
    Rolling,
}

#[derive(Clone, Copy)]
pub struct BlockingEngineConfig {
    pub prefetch: BlockingPrefetch,
    pub fail_fast: bool,
}

pub trait BlockingWorker: Send {
    fn read_chunk(&self, chunk: &ChunkInfo) -> Result<Vec<u8>, String>;
    fn upload_chunk(
        &self,
        url: &str,
        data: &[u8],
        part_number: u32,
        file_key: &str,
    ) -> Result<String, String>;
}

pub trait BlockingPlatformAdapter: Send + Sync + 'static {
    fn is_paused(&self) -> bool {
        false
    }

    fn is_current_run(&self) -> bool {
        true
    }

    fn fetch_urls(&self, parts: &[u32]) -> Result<HashMap<u32, String>, String>;

    fn make_worker(&self) -> Result<Box<dyn BlockingWorker>, String>;

    fn on_missing_url(&self, _part_number: u32) {}

    fn on_read_error(&self, _part_number: u32, _error: &str) {}

    fn on_upload_error(&self, _part_number: u32, _error: &str) {}
}

pub fn run_blocking_upload(
    adapter: Arc<dyn BlockingPlatformAdapter>,
    config: BlockingEngineConfig,
    file_key: &str,
    part_size: u64,
    file_size: u64,
    parts_to_upload: Vec<u32>,
) -> Result<Vec<ChunkUploadResult>, String> {
    let started_at = std::time::Instant::now();
    let completed_parts = Arc::new(Mutex::new(Vec::<ChunkUploadResult>::new()));
    let url_pool: Arc<Mutex<HashMap<u32, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let parts_arc = Arc::new(parts_to_upload);
    let parts_len = parts_arc.len();

    match config.prefetch {
        BlockingPrefetch::AllUpfront => {
            url_pool
                .lock()
                .unwrap()
                .extend(adapter.fetch_urls(&parts_arc[..])?);
        }
        BlockingPrefetch::Rolling => {
            let initial = &parts_arc[..MAX_CONCURRENT_UPLOADS.min(parts_len)];
            if !initial.is_empty() {
                url_pool
                    .lock()
                    .unwrap()
                    .extend(adapter.fetch_urls(initial)?);
            }
        }
    }

    let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(MAX_CONCURRENT_UPLOADS);
    let rx = Arc::new(Mutex::new(rx));
    let abort = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let error_abort = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let file_key_owned = file_key.to_string();

    let mut handles = vec![];
    for _ in 0..MAX_CONCURRENT_UPLOADS {
        let rx = rx.clone();
        let parts = parts_arc.clone();
        let url_pool = url_pool.clone();
        let completed = completed_parts.clone();
        let abort = abort.clone();
        let error_abort = error_abort.clone();
        let adapter = adapter.clone();
        let file_key_str = file_key_owned.clone();

        let handle = thread::spawn(move || {
            let worker = match adapter.make_worker() {
                Ok(worker) => worker,
                Err(error) => {
                    error_abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    adapter.on_upload_error(0, &error);
                    return;
                }
            };

            loop {
                let part_idx = match rx.lock().unwrap().recv() {
                    Ok(i) => i,
                    Err(_) => break,
                };

                if adapter.is_paused() || !adapter.is_current_run() {
                    abort.store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }

                let part_number = parts[part_idx];
                let url = match url_pool.lock().unwrap().get(&part_number).cloned() {
                    Some(url) => url,
                    None => {
                        adapter.on_missing_url(part_number);
                        if config.fail_fast {
                            abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            error_abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        continue;
                    }
                };

                let chunk_info = ChunkInfo {
                    part_number,
                    start_pos: (part_number as u64 - 1) * part_size,
                    chunk_size: upload::part_size_for(part_number, part_size, file_size),
                    url: url.clone(),
                };

                let chunk = match worker.read_chunk(&chunk_info) {
                    Ok(data) => data,
                    Err(error) => {
                        adapter.on_read_error(part_number, &error);
                        if config.fail_fast {
                            abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            if !adapter.is_paused()
                                && !error.contains("paused")
                                && !error.contains("Interrupted")
                                && !error.contains("stale run")
                            {
                                error_abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            break;
                        }
                        continue;
                    }
                };

                let etag = match worker.upload_chunk(&url, &chunk, part_number, &file_key_str) {
                    Ok(etag) => etag,
                    Err(error) => {
                        adapter.on_upload_error(part_number, &error);
                        if config.fail_fast {
                            abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            if !adapter.is_paused()
                                && !error.contains("paused")
                                && !error.contains("Interrupted")
                                && !error.contains("stale run")
                            {
                                error_abort.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            break;
                        }
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

    // Drop the main rx reference so that when all worker threads exit (after aborting),
    // the Receiver is disconnected and a blocked tx.send() returns Err rather than
    // hanging forever. Workers hold the remaining Arc clones and are unaffected.
    drop(rx);

    for (idx, _) in parts_arc.iter().enumerate() {
        if let BlockingPrefetch::Rolling = config.prefetch {
            if idx > 0 && idx % MAX_CONCURRENT_UPLOADS == 0 && idx < parts_len {
                let batch_end = (idx + MAX_CONCURRENT_UPLOADS).min(parts_len);
                let next: Vec<u32> = parts_arc[idx..batch_end].to_vec();
                if let Ok(new_urls) = adapter.fetch_urls(&next) {
                    url_pool.lock().unwrap().extend(new_urls);
                }
            }
        }

        if adapter.is_paused()
            || !adapter.is_current_run()
            || abort.load(std::sync::atomic::Ordering::Relaxed)
        {
            break;
        }
        tx.send(idx).ok();
    }
    drop(tx);

    // For pause/resume races we intentionally do not wait for old worker threads to finish.
    // On iOS, in-flight NSURLSession requests can take a while to terminate even after pause.
    // Waiting here would block the resumed run from starting and eventually calling complete.
    let should_return_early_for_pause =
        adapter.is_paused()
            || !adapter.is_current_run()
            || (abort.load(std::sync::atomic::Ordering::Relaxed)
                && !error_abort.load(std::sync::atomic::Ordering::Relaxed));
    if should_return_early_for_pause {
        eprintln!(
            "[S3BgUploader] run_blocking_upload early-exit for {} after {}ms (paused={}, current_run={}, abort={}, error_abort={})",
            file_key,
            started_at.elapsed().as_millis(),
            adapter.is_paused(),
            adapter.is_current_run(),
            abort.load(std::sync::atomic::Ordering::Relaxed),
            error_abort.load(std::sync::atomic::Ordering::Relaxed),
        );
        return Err("Upload paused".to_string());
    }

    let join_started_at = std::time::Instant::now();
    eprintln!(
        "[S3BgUploader] run_blocking_upload joining workers for {} after {}ms",
        file_key,
        started_at.elapsed().as_millis()
    );
    for handle in handles {
        handle.join().ok();
    }
    eprintln!(
        "[S3BgUploader] run_blocking_upload joined workers for {} in {}ms (total {}ms)",
        file_key,
        join_started_at.elapsed().as_millis(),
        started_at.elapsed().as_millis()
    );

    if adapter.is_paused()
        || !adapter.is_current_run()
        || (abort.load(std::sync::atomic::Ordering::Relaxed)
            && !error_abort.load(std::sync::atomic::Ordering::Relaxed))
    {
        return Err("Upload paused".to_string());
    }

    let results = completed_parts.lock().unwrap().clone();
    if results.len() != parts_arc.len() {
        return Err(format!(
            "Only {}/{} parts uploaded successfully",
            results.len(),
            parts_arc.len()
        ));
    }
    Ok(results)
}
