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
use crate::core::clean_etag;
use crate::core::encryption::{ArcEncryptor, ChunkEncryptor};
use crate::core::runtime;
use crate::core::session::{self, UploadState};
use crate::core::upload::PreparedUpload;
use crate::core::upload_orchestrator::{self, UploadBackend, UploadOutcome};
use crate::native::upload_engine::{
    self, BlockingEngineConfig, BlockingPlatformAdapter, BlockingPrefetch, BlockingWorker,
};
use crate::native::NativeApiClient;

pub(super) fn run_upload(file_key: &str, raw_fd: RawFd) {
    use crate::core::session;
    let (run_version, encryption_config) = match session::session().files.get(file_key) {
        Some(e) => (e.run_version, e.encryption_config.clone()),
        None => {
            unsafe { libc::close(raw_fd) };
            return;
        }
    };

    let backend = AndroidUploadBackend {
        file_key: file_key.to_string(),
        raw_fd,
        run_version,
        encryption_config,
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
                .map(|e| {
                    matches!(
                        e.state,
                        UploadState::Paused | UploadState::NotStarted | UploadState::Initialized
                    )
                })
                .unwrap_or(false);
            if should_reenqueue {
                // Re-enqueue at the front so this partially-uploaded file resumes before
                // not-yet-started files that are still waiting in the queue.
                enqueue_front(file_key.to_string(), raw_fd);
            } else {
                unsafe { libc::close(raw_fd) };
            }
        }
        UploadOutcome::Failed(ref e) => {
            // The reason is otherwise dropped here, which makes Android failures invisible.
            log::error!("upload failed for {}: {}", file_key, e);
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
    encryption_config: Option<ArcEncryptor>,
    api: NativeApiClient<AndroidNetwork>,
}

#[async_trait(?Send)]
impl UploadBackend for AndroidUploadBackend {
    type Notifier = AndroidProgressNotifier;

    fn encryption_config(&self) -> Option<&ArcEncryptor> {
        self.encryption_config.as_ref()
    }

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

    fn ciphertext_overhead(&self) -> u64 {
        active_encryptor(self.encryption_config.as_ref())
            .map(|e| e.ciphertext_overhead() as u64)
            .unwrap_or(0)
    }

    fn has_footer(&self) -> bool {
        active_encryptor(self.encryption_config.as_ref())
            .map(|e| e.has_footer())
            .unwrap_or(false)
    }

    /// Bytes the descriptor occupies in the last data part (0 when there is none).
    fn footer_len(&self) -> u64 {
        active_encryptor(self.encryption_config.as_ref())
            .map(|e| e.footer_len() as u64)
            .unwrap_or(0)
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
            encryption_config: self.encryption_config.clone(),
            total_data_parts: prepared.total_parts,
            effective_part_size: prepared.part_size,
            has_footer: self.has_footer(),
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
    encryption_config: Option<ArcEncryptor>,
    total_data_parts: u32,
    effective_part_size: u64,
    has_footer: bool,
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
            NativeApiClient {
                network: self.network.clone(),
            }
            .fetch_upload_urls_batch(&self.file_key, &self.upload_id, parts),
        )
    }

    fn make_worker(&self) -> Result<Box<dyn BlockingWorker>, String> {
        Ok(Box::new(AndroidWorker {
            client: build_client(),
            raw_fd: self.raw_fd,
            run_version: self.run_version,
            encryption_config: self.encryption_config.clone(),
            total_data_parts: self.total_data_parts,
            effective_part_size: self.effective_part_size,
            has_footer: self.has_footer,
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
    encryption_config: Option<ArcEncryptor>,
    total_data_parts: u32,
    effective_part_size: u64,
    /// Whether the active encryptor's format ends with a descriptor (appended to the
    /// final data part — never an S3 part of its own).
    has_footer: bool,
}

/// The encryptor that will actually encrypt chunks: a host-registered custom
/// encryptor takes precedence over the per-file built-in config.
fn active_encryptor<'a>(config: Option<&'a ArcEncryptor>) -> Option<&'a (dyn ChunkEncryptor + 'a)> {
    // Only files the app marked as encrypted are encrypted at all; for those, a
    // registered custom encryptor replaces the built-in one. Keeping the per-file
    // config as the gate means a plain `uploadFile` stays plaintext instead of
    // being silently encrypted by a globally registered encryptor.
    // The per-file config is authoritative: it already holds either the host
    // encryptor built from this upload's key or the built-in one.
    config.map(|c| c.as_ref() as &'a dyn ChunkEncryptor)
}

/// Read a single chunk from the file using `pread` (so concurrent workers
/// don't corrupt each other's file position). Returns the raw plaintext bytes.
fn read_chunk_from_fd(fd: RawFd, chunk: &ChunkInfo) -> Result<Vec<u8>, String> {
    let mut buffer = vec![0u8; chunk.chunk_size as usize];
    let mut total_read: usize = 0;
    while total_read < chunk.chunk_size as usize {
        let remaining = chunk.chunk_size as usize - total_read;
        let offset = (chunk.start_pos + total_read as u64) as libc::off_t;
        let n = unsafe {
            libc::pread(
                fd,
                buffer[total_read..].as_mut_ptr() as *mut libc::c_void,
                remaining,
                offset,
            )
        };
        match n {
            -1 => return Err(std::io::Error::last_os_error().to_string()),
            0 => break,
            n => total_read += n as usize,
        }
    }
    buffer.truncate(total_read);
    Ok(buffer)
}

impl BlockingWorker for AndroidWorker {
    fn read_chunk(&self, chunk: &ChunkInfo) -> Result<Vec<u8>, String> {
        // A host-registered custom encryptor wins over the per-file built-in config.
        let enc = match active_encryptor(self.encryption_config.as_ref()) {
            Some(enc) => enc,
            // No encryption configured: upload raw plaintext.
            None => return read_chunk_from_fd(self.raw_fd, chunk),
        };
        let mut out = enc.encrypt_chunk(&read_chunk_from_fd(self.raw_fd, chunk)?, chunk.part_number);
        // The descriptor rides inside the final data part — a trailing part of its
        // own would demote this one below S3's 5 MiB minimum (see append_descriptor).
        crate::core::upload::append_descriptor(
            &mut out,
            enc,
            chunk.part_number,
            self.total_data_parts,
            self.effective_part_size,
            self.has_footer,
        );
        Ok(out)
    }

    fn upload_chunk(
        &self,
        url: &str,
        data: &[u8],
        part_number: u32,
        file_key: &str,
    ) -> Result<String, String> {
        upload_chunk_with_retry(
            &self.client,
            url,
            data,
            part_number,
            file_key,
            self.run_version,
        )
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
    // Short backoff: a part whose response was lost is re-sent immediately instead of
    // after the 1s/2s/4s/8s API ladder (see RetryPolicy::part_upload).
    let policy = crate::core::retry::RetryPolicy::part_upload();
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

            // Reset this part to zero before every attempt: the previous attempt's bytes
            // were never acknowledged and the body is sent again from the beginning, so
            // the bar drops back to the stored parts and climbs with the re-upload.
            runtime::update_in_flight(
                androidProgress::progress_manager(),
                &file_key,
                part_number,
                0,
            );

            let progress_reader =
                ProgressReader::new(data.to_vec(), file_key.clone(), run_version, part_number);
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
