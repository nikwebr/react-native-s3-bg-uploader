use std::fs::File;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::Arc;
use std::thread;

use async_trait::async_trait;

use crate::android::progress::{self as androidProgress, AndroidProgressNotifier, ProgressReader};
use crate::android::AndroidNetwork;
use crate::android::{build_client, dup_fd, enqueue, enqueue_front, PAUSE_FLAG};
use crate::core::api::ApiClient;
use crate::core::chunk::ChunkInfo;
use crate::core::session::{self, UploadState};
use crate::native::NativeApiClient;
use crate::core::runtime;
use crate::core::upload::PreparedUpload;
use crate::native::upload_engine::{
    self, BlockingEngineConfig, BlockingPlatformAdapter, BlockingPrefetch, BlockingWorker,
};
use crate::core::upload_orchestrator::{self, UploadBackend, UploadOutcome};
use crate::core::{clean_etag, MAX_RETRIES};

pub(super) fn run_upload(file_key: &str, raw_fd: RawFd) {
    use crate::core::session;
    let run_version = match session::session().files.get(file_key) {
        Some(e) => e.run_version,
        None => {
            unsafe { libc::close(raw_fd) };
            return;
        }
    };

    let backend = AndroidUploadBackend {
        file_key: file_key.to_string(),
        raw_fd,
        run_version,
        api: NativeApiClient {
            network: AndroidNetwork {
                client: build_client(),
            },
        },
    };

    let outcome = pollster::block_on(upload_orchestrator::run_upload(&backend));

    match outcome {
        UploadOutcome::Paused => {
            let should_reenqueue = session::session()
                .files
                .get(file_key)
                .map(|e| matches!(e.state, UploadState::Paused | UploadState::NotStarted | UploadState::Initialized))
                .unwrap_or(false);
            if should_reenqueue {
                // Re-enqueue at the front so this partially-uploaded file resumes before
                // not-yet-started files that are still waiting in the queue.
                enqueue_front(file_key.to_string(), raw_fd);
            } else {
                unsafe { libc::close(raw_fd) };
            }
        }
        UploadOutcome::Failed(_) => {
            // Keep the fd alive so nativeResumeAll can re-enqueue this file for retry.
            crate::android::FAILED_FDS
                .lock()
                .unwrap()
                .insert(file_key.to_string(), raw_fd);
        }
        UploadOutcome::Completed => {
            unsafe { libc::close(raw_fd) };
        }
    }
}

struct AndroidUploadBackend {
    file_key: String,
    raw_fd: RawFd,
    run_version: u64,
    api: NativeApiClient<AndroidNetwork>,
}

#[async_trait(?Send)]
impl UploadBackend for AndroidUploadBackend {
    type Notifier = AndroidProgressNotifier;

    fn progress_manager(&self) -> &crate::core::progress::ProgressManager<Self::Notifier> {
        androidProgress::progress_manager()
    }

    fn file_key(&self) -> &str {
        &self.file_key
    }

    fn is_paused(&self) -> bool {
        PAUSE_FLAG.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn is_current_run(&self) -> bool {
        session::is_current_run(&self.file_key, self.run_version)
    }

    fn total_bytes(&self) -> Result<u64, String> {
        let file = unsafe { File::from_raw_fd(dup_fd(self.raw_fd).map_err(|e| e.to_string())?) };
        Ok(file.metadata().map_err(|e| e.to_string())?.len())
    }

    async fn upload_parts(
        &self,
        prepared: &PreparedUpload,
        total_bytes: u64,
    ) -> Result<Vec<crate::core::ChunkUploadResult>, String> {
        let adapter = Arc::new(AndroidAdapter {
            raw_fd: self.raw_fd,
            file_key: self.file_key.clone(),
            upload_id: prepared.upload_id.clone(),
            run_version: self.run_version,
            network: self.api.network.clone(),
        });
        upload_engine::run_blocking_upload(
            adapter,
            BlockingEngineConfig {
                prefetch: BlockingPrefetch::Rolling,
                fail_fast: true,
            },
            &self.file_key,
            prepared.part_size,
            total_bytes,
            prepared.remaining_parts.clone(),
        )
    }

    async fn complete_upload(
        &self,
        upload_id: &str,
        results: Vec<crate::core::ChunkUploadResult>,
    ) -> Result<(), String> {
        self.api
            .complete_upload(&self.file_key, upload_id, results)
            .await
    }
}

struct AndroidAdapter {
    raw_fd: RawFd,
    file_key: String,
    upload_id: String,
    run_version: u64,
    network: AndroidNetwork,
}

impl BlockingPlatformAdapter for AndroidAdapter {
    fn is_paused(&self) -> bool {
        PAUSE_FLAG.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn is_current_run(&self) -> bool {
        session::is_current_run(&self.file_key, self.run_version)
    }

    fn fetch_urls(&self, parts: &[u32]) -> Result<std::collections::HashMap<u32, String>, String> {
        pollster::block_on(
            NativeApiClient { network: self.network.clone() }
                .fetch_upload_urls_batch(&self.file_key, &self.upload_id, parts),
        )
    }

    fn make_worker(&self) -> Result<Box<dyn BlockingWorker>, String> {
        Ok(Box::new(AndroidWorker {
            client: build_client(),
            raw_fd: self.raw_fd,
            run_version: self.run_version,
        }))
    }

    fn on_missing_url(&self, part_number: u32) {
        log::error!("No URL for part {}", part_number);
    }

    fn on_read_error(&self, part_number: u32, error: &str) {
        log::error!("Failed to read chunk {}: {}", part_number, error);
    }

    fn on_upload_error(&self, part_number: u32, error: &str) {
        log::error!("Failed to upload part {}: {}", part_number, error);
    }
}

struct AndroidWorker {
    client: reqwest::blocking::Client,
    raw_fd: RawFd,
    run_version: u64,
}

impl BlockingWorker for AndroidWorker {
    fn read_chunk(&self, chunk: &ChunkInfo) -> Result<Vec<u8>, String> {
        // Use pread so concurrent workers don't corrupt each other's file position.
        // libc::dup shares the underlying file offset, so BufReader+seek across
        // multiple threads would interleave seeks and produce wrong/truncated data.
        let mut buffer = vec![0u8; chunk.chunk_size as usize];
        let mut total_read: usize = 0;
        while total_read < chunk.chunk_size as usize {
            let remaining = chunk.chunk_size as usize - total_read;
            let offset = (chunk.start_pos + total_read as u64) as libc::off_t;
            let n = unsafe {
                libc::pread(
                    self.raw_fd,
                    buffer[total_read..].as_mut_ptr() as *mut libc::c_void,
                    remaining,
                    offset,
                )
            };
            match n {
                -1 => return Err(std::io::Error::last_os_error().to_string()),
                0 => break, // EOF
                n => total_read += n as usize,
            }
        }
        buffer.truncate(total_read);
        Ok(buffer)
    }

    fn upload_chunk(
        &self,
        url: &str,
        data: &[u8],
        part_number: u32,
        file_key: &str,
    ) -> Result<String, String> {
        upload_chunk_with_retry(&self.client, url, data, part_number, file_key, self.run_version)
            .map_err(|e| e.to_string())
    }
}

fn upload_chunk_with_retry(
    client: &reqwest::blocking::Client,
    url: &str,
    data: &[u8],
    part_number: u32,
    file_key: &str,
    run_version: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let chunk_size = data.len() as u64;
    let policy = crate::core::retry::RetryPolicy::new(MAX_RETRIES);
    let file_key = file_key.to_string();

    crate::core::retry::run_with_retry_string(
        &policy,
        |_attempt| {
            if crate::android::PAUSE_FLAG.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("paused".to_string());
            }
            if !session::is_current_run(&file_key, run_version) {
                return Err("stale run".to_string());
            }

            runtime::update_in_flight(
                androidProgress::progress_manager(),
                &file_key,
                part_number,
                0,
            );

            let progress_reader = ProgressReader::new(data.to_vec(), file_key.clone(), run_version, part_number);
            let response = match client
                .put(url)
                .header("Content-Length", chunk_size.to_string())
                .body(reqwest::blocking::Body::new(progress_reader))
                .send()
            {
                Ok(r) => r,
                Err(e) => {
                    if crate::android::PAUSE_FLAG.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err("paused".to_string());
                    }
                    if !session::is_current_run(&file_key, run_version) {
                        return Err("stale run".to_string());
                    }
                    return Err(format!("{:?}", e));
                }
            };

            let etag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(clean_etag)
                .ok_or_else(|| "No ETag in response".to_string())?;

            // Check staleness after a successful response — a pause+resume may have
            // started a new run while this request was in-flight. Without this check
            // the old worker would double-count the part in the ProgressManager.
            if !session::is_current_run(&file_key, run_version) {
                return Err("stale run".to_string());
            }

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
            if !crate::android::PAUSE_FLAG.load(std::sync::atomic::Ordering::Relaxed) {
                thread::sleep(std::time::Duration::from_millis(delay_ms as u64));
            }
        },
    )
    .map_err(|e| e.into())
}
