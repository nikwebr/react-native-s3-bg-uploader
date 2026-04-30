use async_trait::async_trait;

use crate::core::progress::{ProgressManager, ProgressNotifier};
use crate::core::runtime;
use crate::core::session::{self, GlobalUploaderState, UploadState};
use crate::core::upload::{self, PreparedUpload};
use crate::core::ChunkUploadResult;

pub enum UploadOutcome {
    Completed,
    Paused,
    Failed(String),
}

#[async_trait(?Send)]
pub trait UploadBackend {
    type Notifier: ProgressNotifier;

    fn progress_manager(&self) -> &ProgressManager<Self::Notifier>;

    fn file_key(&self) -> &str;

    fn is_paused(&self) -> bool;

    fn is_current_run(&self) -> bool {
        true
    }

    fn total_bytes(&self) -> Result<u64, String>;

    async fn upload_parts(
        &self,
        prepared: &PreparedUpload,
        total_bytes: u64,
    ) -> Result<Vec<ChunkUploadResult>, String>;

    async fn complete_upload(
        &self,
        upload_id: &str,
        results: Vec<ChunkUploadResult>,
    ) -> Result<(), String>;

    fn on_session_completed(&self) {}
}

pub async fn run_upload<B: UploadBackend>(backend: &B) -> UploadOutcome {
    let file_key = backend.file_key().to_string();

    runtime::mark_state_persist(&file_key, UploadState::Running);

    let outcome = run_upload_inner(backend).await;

    match &outcome {
        UploadOutcome::Completed => {
            runtime::mark_state_persist(&file_key, UploadState::Completed);
            runtime::set_status(
                backend.progress_manager(),
                &file_key,
                UploadState::Completed,
            );
            if session::session().global_state == GlobalUploaderState::Completed {
                session::clear_session();
                backend.on_session_completed();
            }
        }
        UploadOutcome::Paused => {
            if session::session()
                .files
                .get(&file_key)
                .map(|e| e.state == UploadState::NotStarted)
                .unwrap_or(false)
            {
                // resume_all() already reset the state; skip persisting Paused over it.
            } else {
                runtime::mark_state_persist(&file_key, UploadState::Paused);
            }
        }
        UploadOutcome::Failed(e) => {
            runtime::mark_state_persist(&file_key, UploadState::Failed);
            runtime::set_status(
                backend.progress_manager(),
                &file_key,
                UploadState::Failed,
            );
            log_error(&file_key, e);
        }
    }

    outcome
}

async fn run_upload_inner<B: UploadBackend>(backend: &B) -> UploadOutcome {
    let file_key = backend.file_key().to_string();
    let started_at = std::time::Instant::now();

    let total_bytes = match backend.total_bytes() {
        Ok(b) => b,
        Err(e) => return UploadOutcome::Failed(e),
    };
    let prepared = match upload::prepare_upload(&file_key, total_bytes) {
        Ok(p) => p,
        Err(e) => return UploadOutcome::Failed(e),
    };

    session::session().set_file_uploaded_bytes(&file_key, prepared.committed_bytes);

    if prepared.remaining_parts.is_empty() {
        let all_completed =
            upload::combine_upload_results(prepared.completed_etags.clone(), Vec::new());
        return match backend
            .complete_upload(&prepared.upload_id, all_completed)
            .await
        {
            Ok(_) => UploadOutcome::Completed,
            Err(e) if e.contains("stale run") => UploadOutcome::Paused,
            Err(e) => UploadOutcome::Failed(e),
        };
    }

    runtime::init_progress(
        backend.progress_manager(),
        &file_key,
        total_bytes,
        prepared.total_parts,
        prepared.committed_bytes,
        prepared.done_parts.len() as u32,
    );

    let new_results = match backend.upload_parts(&prepared, total_bytes).await {
        Ok(r) => r,
        Err(e)
            if backend.is_paused() || e.contains("paused") || e.contains("stale run") =>
        {
            return UploadOutcome::Paused;
        }
        Err(e) => {
            // resume_all() may have reset the file to NotStarted while upload_parts was
            // in-flight (rapid pause→resume). In that case treat this as Paused so the
            // file gets re-enqueued from the correct path.
            let was_reset = session::session()
                .files
                .get(&file_key)
                .map(|e| e.state == UploadState::NotStarted)
                .unwrap_or(false);
            if was_reset {
                return UploadOutcome::Paused;
            }
            return UploadOutcome::Failed(e);
        }
    };

    let all_results =
        upload::combine_upload_results(prepared.completed_etags.clone(), new_results);
    if !backend.is_current_run() {
        return UploadOutcome::Paused;
    }
    eprintln!(
        "[S3BgUploader] complete_upload starting for {} after {}ms with {} parts",
        file_key,
        started_at.elapsed().as_millis(),
        all_results.len()
    );
    match backend.complete_upload(&prepared.upload_id, all_results).await {
        Ok(_) => {
            eprintln!(
                "[S3BgUploader] complete_upload finished for {} after {}ms",
                file_key,
                started_at.elapsed().as_millis()
            );
            UploadOutcome::Completed
        }
        Err(e) if e.contains("stale run") => UploadOutcome::Paused,
        Err(e) => UploadOutcome::Failed(e),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn log_error(file_key: &str, e: &str) {
    eprintln!("[S3BgUploader] Upload failed for {}: {}", file_key, e);
}

#[cfg(target_arch = "wasm32")]
fn log_error(_file_key: &str, _e: &str) {}
