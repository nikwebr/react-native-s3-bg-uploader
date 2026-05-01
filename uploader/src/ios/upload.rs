use std::fs::File;
use std::io::BufReader;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;

use async_trait::async_trait;

use crate::core::api::ApiClient;
use crate::core::chunk::ChunkInfo;
use crate::native::NativeApiClient;
use crate::core::runtime;
use crate::core::session::{self, UploadState};
use crate::core::upload::PreparedUpload;
use crate::native::upload_engine::{
    self, BlockingEngineConfig, BlockingPlatformAdapter, BlockingPrefetch, BlockingWorker,
};
use crate::core::upload_orchestrator::{self, UploadBackend, UploadOutcome};
use crate::core::{clean_etag, MAX_RETRIES};
use crate::ios::progress::{self as iosProgress, IosProgressNotifier, ProgressReader};
use crate::ios::{enqueue_key, init_nyquest, IosNetwork, PAUSE_FLAG};

pub(super) fn run_upload(file_key: &str) {
    init_nyquest();

    let (file_path, run_version) = {
        let sess = session::session();
        match sess.files.get(file_key) {
            Some(e)
                if matches!(
                    e.state,
                    UploadState::NotStarted | UploadState::Paused | UploadState::Failed
                ) =>
            {
                (e.file_path.clone(), e.run_version)
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
        api: NativeApiClient {
            network: IosNetwork { client },
        },
    };

    match pollster::block_on(upload_orchestrator::run_upload(&backend)) {
        UploadOutcome::Completed => {
            if let Some(next_key) = session::session().next_pending_file() {
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
        self.api.complete_upload(&self.file_key, upload_id, results).await
    }
}

struct IosAdapter {
    file_path: String,
    file_key: String,
    upload_id: String,
    run_version: u64,
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
            NativeApiClient { network: self.network.clone() }
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
}

impl BlockingWorker for IosWorker {
    fn read_chunk(&self, chunk: &ChunkInfo) -> Result<Vec<u8>, String> {
        let file = File::open(&self.file_path).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(file);
        chunk.read(&mut reader).map_err(|e| e.to_string())
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

            let progress_reader = ProgressReader::new(data.to_vec(), file_key.clone(), run_version, part_number);
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
