use async_trait::async_trait;

use crate::core::encryption::ArcEncryptor;
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

    fn encryption_config(&self) -> Option<&ArcEncryptor> {
        None
    }

    /// Bytes added per chunk by the active encryptor (0 for plain uploads).
    /// Used to compute the effective plaintext part size so ciphertext fits
    /// exactly into the presigned-URL slot.
    fn ciphertext_overhead(&self) -> u64 {
        0
    }

    /// Whether the active encryptor's wire format ends with a synthesised
    /// descriptor. Defaults to `overhead > 0` (the built-in AEAD layout).
    fn has_footer(&self) -> bool {
        self.ciphertext_overhead() > 0
    }

    /// Bytes the descriptor occupies in the last data part (0 when there is none).
    /// Defaults to 12 bytes of metadata plus the encryptor's overhead, matching
    /// `ChunkEncryptor::footer_len()`.
    fn footer_len(&self) -> u64 {
        if self.has_footer() {
            12 + self.ciphertext_overhead()
        } else {
            0
        }
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
                || !backend.is_current_run()
            {
                // resume_all() already reset the state, or a new run has since started.
                // Don't overwrite the new run's state or progress with Paused.
            } else {
                runtime::mark_state_persist(&file_key, UploadState::Paused);
                // Ensure the ProgressManager reflects Paused even when pause_all ran
                // before init_progress created the ProgressManager entry.
                runtime::set_status(backend.progress_manager(), &file_key, UploadState::Paused);
            }
        }
        UploadOutcome::Failed(e) => {
            runtime::mark_state_persist(&file_key, UploadState::Failed);
            runtime::set_status(backend.progress_manager(), &file_key, UploadState::Failed);
            log_error(&file_key, e);
        }
    }

    outcome
}

async fn run_upload_inner<B: UploadBackend>(backend: &B) -> UploadOutcome {
    let file_key = backend.file_key().to_string();

    let total_bytes = match backend.total_bytes() {
        Ok(b) => b,
        Err(e) => return UploadOutcome::Failed(e),
    };
    let prepared = match upload::prepare_upload(
        &file_key,
        total_bytes,
        backend.ciphertext_overhead(),
        backend.has_footer(),
        backend.footer_len(),
        backend.encryption_config(),
    ) {
        Ok(p) => p,
        Err(e) => return UploadOutcome::Failed(e),
    };

    session::session().set_file_sizes(&file_key, prepared.total_ciphertext_bytes, total_bytes);
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
        prepared.total_ciphertext_bytes,
        prepared.total_parts,
        prepared.committed_bytes,
        prepared.done_parts.len() as u32,
    );

    let new_results = match backend.upload_parts(&prepared, total_bytes).await {
        Ok(r) => r,
        Err(e) if backend.is_paused() || e.contains("paused") || e.contains("stale run") => {
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

    let all_results = upload::combine_upload_results(prepared.completed_etags.clone(), new_results);
    if !backend.is_current_run() {
        return UploadOutcome::Paused;
    }
    match backend
        .complete_upload(&prepared.upload_id, all_results)
        .await
    {
        Ok(_) => UploadOutcome::Completed,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::encryption::XChaCha20Poly1305Encryptor;
    use crate::core::progress::NoOpNotifier;
    use crate::core::session::FileEntry;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct MissingEncryptionBackend {
        key: String,
        progress: ProgressManager<NoOpNotifier>,
    }

    #[async_trait(?Send)]
    impl UploadBackend for MissingEncryptionBackend {
        type Notifier = NoOpNotifier;

        fn progress_manager(&self) -> &ProgressManager<NoOpNotifier> {
            &self.progress
        }

        fn file_key(&self) -> &str {
            &self.key
        }

        fn is_paused(&self) -> bool {
            false
        }

        fn total_bytes(&self) -> Result<u64, String> {
            Ok(2048)
        }

        async fn upload_parts(
            &self,
            _: &PreparedUpload,
            _: u64,
        ) -> Result<Vec<ChunkUploadResult>, String> {
            panic!("must not upload plaintext when the worker has no encryption config")
        }

        async fn complete_upload(&self, _: &str, _: Vec<ChunkUploadResult>) -> Result<(), String> {
            panic!("must not complete an upload without its encryption config")
        }
    }

    #[test]
    fn background_resume_rejects_missing_and_stale_encryption_snapshots() {
        for (case, completed_parts, current_config) in [
            ("new", 0, false),
            ("partial", 1, false),
            ("all-parts", 2, false),
            ("stale-worker", 1, true),
        ] {
            let key = format!("encryption-guard-{case}");
            let mut entry = FileEntry::new(
                key.clone(),
                key.clone(),
                key.clone(),
                "file".into(),
                "file".into(),
                2048,
                HashMap::new(),
            );
            entry.encrypted = true;
            entry.upload_id = Some("upload".into());
            entry.part_size = Some(1024);
            entry.completed_chunk_etags = (1..=completed_parts)
                .map(|part| (part, format!("etag-{part}")))
                .collect();
            if current_config {
                entry.encryption_config = Some(Arc::new(
                    XChaCha20Poly1305Encryptor::from_hex(&"ab".repeat(32), &"cd".repeat(24))
                        .unwrap(),
                ));
            }
            session::session().files.insert(key.clone(), entry);
            let backend = MissingEncryptionBackend {
                key: key.clone(),
                progress: ProgressManager::new(NoOpNotifier),
            };
            let outcome = pollster::block_on(run_upload(&backend));
            assert!(
                matches!(outcome, UploadOutcome::Failed(ref e) if e.contains("encryptAndUploadFile"))
            );
            let entry = session::session().files.remove(&key).unwrap();
            assert_eq!(entry.state, UploadState::Failed);
            assert_eq!(entry.completed_chunk_etags.len(), completed_parts as usize);
        }
    }
}
