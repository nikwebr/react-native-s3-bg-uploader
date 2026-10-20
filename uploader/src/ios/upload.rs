use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;

use async_trait::async_trait;

use crate::core::api::ApiClient;
use crate::core::chunk::ChunkInfo;
use crate::core::encryption::{ArcEncryptor, ChunkEncryptor};
use crate::core::runtime;
use crate::core::session::{self, UploadState};
use crate::core::upload::PreparedUpload;
use crate::core::upload_orchestrator::{self, UploadBackend, UploadOutcome};
use crate::core::{clean_etag, MAX_RETRIES};
use crate::ios::progress::{self as iosProgress, IosProgressNotifier, ProgressReader};
use crate::ios::{enqueue_key, init_nyquest, IosNetwork, PAUSE_FLAG};
use crate::native::upload_engine::{
    self, BlockingEngineConfig, BlockingPlatformAdapter, BlockingPrefetch, BlockingWorker,
};
use crate::native::NativeApiClient;

pub(super) fn run_upload(file_key: &str) {
    init_nyquest();

    let (file_path, run_version, encryption_config) = {
        let sess = session::session();
        match sess.files.get(file_key) {
            Some(e)
                if matches!(
                    e.state,
                    UploadState::NotStarted
                        | UploadState::Initialized
                        | UploadState::Running  // app was killed mid-upload; resume from where it left off
                        | UploadState::Paused
                        | UploadState::Failed
                ) =>
            {
                (
                    e.file_path.clone(),
                    e.run_version,
                    e.encryption_config.clone(),
                )
            }
            None => return,
            _ => return,
        }
    };

    let client = match nyquest::ClientBuilder::default()
        .request_timeout(std::time::Duration::from_secs(300))
        .build_blocking()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to create client: {:?}", e);
            return;
        }
    };
    let backend = IosUploadBackend {
        file_key: file_key.to_string(),
        file_path,
        run_version,
        encryption_config,
        api: NativeApiClient {
            network: IosNetwork { client },
        },
    };

    match pollster::block_on(upload_orchestrator::run_upload(&backend)) {
        UploadOutcome::Completed => {
            let next_key = session::session().next_pending_file();
            if let Some(next_key) = next_key {
                enqueue_key(next_key);
            }
        }
        UploadOutcome::Paused => {
            let should_reenqueue = session::session()
                .files
                .get(file_key)
                .map(|e| e.state == UploadState::Paused)
                .unwrap_or(false);
            // Only re-enqueue if the file is still paused. When resume_all() has already
            // reset it, that path is responsible for scheduling the next run.
            if should_reenqueue {
                enqueue_key(file_key.to_string());
            }
        }
        UploadOutcome::Failed(_) => {}
    }
}

struct IosUploadBackend {
    file_key: String,
    file_path: String,
    run_version: u64,
    encryption_config: Option<ArcEncryptor>,
    api: NativeApiClient<IosNetwork>,
}

#[async_trait(?Send)]
impl UploadBackend for IosUploadBackend {
    type Notifier = IosProgressNotifier;

    fn progress_manager(&self) -> &crate::core::progress::ProgressManager<Self::Notifier> {
        iosProgress::progress_manager()
    }

    fn file_key(&self) -> &str {
        &self.file_key
    }

    fn is_paused(&self) -> bool {
        PAUSE_FLAG.load(Ordering::Relaxed)
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
        let file = File::open(&self.file_path).map_err(|e| e.to_string())?;
        Ok(file.metadata().map_err(|e| e.to_string())?.len())
    }

    async fn upload_parts(
        &self,
        prepared: &PreparedUpload,
        total_bytes: u64,
    ) -> Result<Vec<crate::core::ChunkUploadResult>, String> {
        let adapter = Arc::new(IosAdapter {
            file_path: self.file_path.clone(),
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

struct IosAdapter {
    file_path: String,
    file_key: String,
    upload_id: String,
    run_version: u64,
    encryption_config: Option<ArcEncryptor>,
    total_data_parts: u32,
    effective_part_size: u64,
    has_footer: bool,
    network: IosNetwork,
}

impl BlockingPlatformAdapter for IosAdapter {
    fn is_paused(&self) -> bool {
        PAUSE_FLAG.load(Ordering::Relaxed)
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
        let client = nyquest::ClientBuilder::default()
            .request_timeout(std::time::Duration::from_secs(300))
            .build_blocking()
            .map_err(|e| format!("Failed to create client in worker thread: {:?}", e))?;
        Ok(Box::new(IosWorker {
            file_path: self.file_path.clone(),
            client,
            run_version: self.run_version,
            encryption_config: self.encryption_config.clone(),
            total_data_parts: self.total_data_parts,
            effective_part_size: self.effective_part_size,
            has_footer: self.has_footer,
        }))
    }

    fn on_missing_url(&self, part_number: u32) {
        eprintln!("No URL for part {}", part_number);
    }

    fn on_read_error(&self, part_number: u32, error: &str) {
        eprintln!("Failed to read chunk {}: {}", part_number, error);
    }

    fn on_upload_error(&self, part_number: u32, error: &str) {
        if !error.contains("paused") && !error.contains("Interrupted") {
            eprintln!("Failed to upload part {}: {}", part_number, error);
        }
    }
}

struct IosWorker {
    file_path: String,
    client: nyquest::BlockingClient,
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

/// Read a single chunk from the file using a BufReader.
fn read_chunk_from_file(file_path: &str, chunk: &ChunkInfo) -> Result<Vec<u8>, String> {
    let file = File::open(file_path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    chunk.read(&mut reader).map_err(|e| e.to_string())
}

impl BlockingWorker for IosWorker {
    fn read_chunk(&self, chunk: &ChunkInfo) -> Result<Vec<u8>, String> {
        // A host-registered custom encryptor wins over the per-file built-in config.
        let enc = match active_encryptor(self.encryption_config.as_ref()) {
            Some(enc) => enc,
            // No encryption configured: upload raw plaintext.
            None => return read_chunk_from_file(&self.file_path, chunk),
        };
        let mut out = enc.encrypt_chunk(
            &read_chunk_from_file(&self.file_path, chunk)?,
            chunk.part_number,
        );
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
    client: &nyquest::BlockingClient,
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
            // Abort immediately if paused — avoids retry backoff delays burning time after pause_all().
            if crate::ios::PAUSE_FLAG.load(Ordering::Relaxed) {
                return Err("paused".to_string());
            }
            if !session::is_current_run(&file_key, run_version) {
                return Err("stale run".to_string());
            }

            runtime::update_in_flight(iosProgress::progress_manager(), &file_key, part_number, 0);

            let progress_reader =
                ProgressReader::new(data.to_vec(), file_key.clone(), run_version, part_number);
            let body = nyquest::blocking::Body::stream(
                progress_reader,
                "application/octet-stream",
                chunk_size,
            );
            let request = nyquest::Request::put(url.to_string()).with_body(body);

            match client.request(request) {
                Ok(response) => match response.get_header("etag") {
                    Ok(etag_vec) if !etag_vec.is_empty() => {
                        if !session::is_current_run(&file_key, run_version) {
                            return Err("stale run".to_string());
                        }
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
                Err(e) => {
                    // If the request failed while we're paused (ProgressReader interrupted
                    // the body stream), return "paused" immediately — no retry needed.
                    if crate::ios::PAUSE_FLAG.load(Ordering::Relaxed) {
                        return Err("paused".to_string());
                    }
                    if !session::is_current_run(&file_key, run_version) {
                        return Err("stale run".to_string());
                    }
                    Err(format!("{:?}", e))
                }
            }
        },
        |attempt, err, delay_ms| {
            eprintln!(
                "Upload attempt {} failed for part {}: {}, retrying in {}ms",
                attempt, part_number, err, delay_ms
            );
            // Skip the sleep entirely when paused — the next attempt will return
            // "paused" immediately anyway, so sleeping is pure waste.
            if !crate::ios::PAUSE_FLAG.load(Ordering::Relaxed) {
                thread::sleep(std::time::Duration::from_millis(delay_ms as u64));
            }
        },
    )
    .map_err(|e| e.into())
}
