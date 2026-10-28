pub mod api;
mod bindings;
pub mod hash;
pub mod js_encryptor;
pub mod progress;
pub mod store;
mod upload;
pub mod upload_engine;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use wasm_bindgen_futures::spawn_local;

use crate::core::encryption::ArcEncryptor;
use crate::core::runtime;
use crate::core::session::{self, UploadState};
use crate::core::upload_orchestrator::UploadOutcome;
use crate::wasm::api::WasmApiClient;
use crate::wasm::progress as wasmProgress;

struct FileQueueEntry {
    file_key: String,
    file: web_sys::File,
}

thread_local! {
    pub(crate) static FILE_QUEUE: RefCell<VecDeque<FileQueueEntry>> = RefCell::new(VecDeque::new());
    pub(crate) static QUEUE_RUNNING: RefCell<bool> = RefCell::new(false);
    pub(crate) static PAUSE_REQUESTED: RefCell<bool> = RefCell::new(false);
    /// Stores the web_sys::File for each pending (hashed but not yet initialized) upload.
    /// Keyed by file_hash. Cleared when start_api succeeds or on cancel_all.
    pub(crate) static PENDING_WASM_FILES: RefCell<HashMap<String, web_sys::File>> = RefCell::new(HashMap::new());
    /// Stores the encryptor for each pending encrypted upload. Keyed by file_hash.
    /// Not persisted — the caller re-provides key+nonce on every call to encryptAndUploadFile.
    pub(crate) static PENDING_WASM_ENCRYPTION: RefCell<HashMap<String, ArcEncryptor>> = RefCell::new(HashMap::new());
}

/// Phase 1 only: hash file, pre-register in session, fire NOT_STARTED callback.
/// Returns the file hash immediately. Phase 2 (start_api → INITIALIZED → enqueue)
/// is deferred to `process_pending_files()`, called from `resume()` in JS.
/// This mirrors the native split so that `uploadFile` always resolves before any
/// INITIALIZED callback fires, preventing a race where the progress matcher can't
/// find the queue item by fileHash yet.
///
/// If the hash already maps to a resumable session entry (e.g. after a page reload),
/// we skip pre-registration and the NOT_STARTED callback entirely — the existing entry
/// stays in the UI and will be resumed via `process_pending_files`.
pub(super) async fn start_and_enqueue(
    file: web_sys::File,
    transfer_id: String,
    user_params: HashMap<String, String>,
) -> Result<String, String> {
    start_and_enqueue_inner(file, transfer_id, user_params, None).await
}

/// Encrypted variant: same as `start_and_enqueue`, but the upload's own key and
/// nonce are handed to whichever scheme is registered (JS factory, or the
/// built-in cipher when none is), exactly like the native platforms.
pub(super) async fn start_and_enqueue_encrypted(
    file: web_sys::File,
    transfer_id: String,
    user_params: HashMap<String, String>,
    key_hex: String,
    nonce_hex: String,
) -> Result<String, String> {
    start_and_enqueue_inner(file, transfer_id, user_params, Some((key_hex, nonce_hex))).await
}

async fn start_and_enqueue_inner(
    file: web_sys::File,
    transfer_id: String,
    user_params: HashMap<String, String>,
    encryption: Option<(String, String)>,
) -> Result<String, String> {
    let file_hash = hash::hash_web_file(&file, &transfer_id)?;

    // Reject duplicates before invoking a factory that could replace an active instance.
    let existing_state = {
        let sess = session::session();
        match sess.find_by_hash(&file_hash) {
            Some(entry) => {
                if entry.state != UploadState::Completed && encryption.is_none() {
                    entry.require_encryption_config(None)?;
                }
                Some((entry.state.clone(), entry.is_resumable()))
            }
            None => None,
        }
    };
    match existing_state {
        Some((UploadState::Completed, _)) => return Ok(file_hash),
        Some((_, false)) => {
            return Err(format!(
                "DUPLICATE_FILE: file with hash {} is already active in this session",
                file_hash
            ));
        }
        _ => {}
    }
    if session::session().pending_files.contains_key(&file_hash) {
        return Err(format!(
            "DUPLICATE_FILE: file with hash {} is already pending in this session",
            file_hash
        ));
    }

    // Build this file's encryptor from its key material. A registered JS factory
    // gets the bytes and keeps one instance per file; the config below is then
    // only the built-in fallback (never used while that instance exists).
    let encryption_config = match encryption.as_ref() {
        Some((key_hex, nonce_hex)) => {
            let key = crate::core::encryption::hex_decode(key_hex, "encryption key")?;
            let nonce = crate::core::encryption::hex_decode(nonce_hex, "encryption nonce")?;
            let config = crate::core::encryption::build_encryptor(key_hex, nonce_hex)?;
            crate::wasm::js_encryptor::create_instance(&file_hash, &key, &nonce)
                .map_err(|e| format!("registerWebEncryptor: {e}"))?;
            Some(config)
        }
        None => {
            crate::wasm::js_encryptor::forget_instance(&file_hash);
            None
        }
    };

    // If this hash already exists in session (resumable or completed) don't create a
    // duplicate UI entry — just re-associate the File object.
    match existing_state {
        Some((UploadState::Completed, _)) => {
            return Ok(file_hash);
        }
        Some((_, true)) => {
            session::session()
                .files_needing_provision
                .remove(&file_hash);
            PENDING_WASM_FILES.with(|m| m.borrow_mut().insert(file_hash.clone(), file));
            if let Some(config) = encryption_config {
                PENDING_WASM_ENCRYPTION.with(|m| m.borrow_mut().insert(file_hash.clone(), config));
            }
            return Ok(file_hash);
        }
        Some(_) => {
            return Err(format!(
                "DUPLICATE_FILE: file with hash {} is already active in this session",
                file_hash
            ));
        }
        None => {}
    }

    session::session().pre_register_file(
        file_hash.clone(),
        transfer_id.clone(),
        String::new(),
        file.name(),
        file.size() as u64,
        user_params,
        encryption_config.clone(),
    )?;

    PENDING_WASM_FILES.with(|m| m.borrow_mut().insert(file_hash.clone(), file));
    if let Some(config) = encryption_config {
        PENDING_WASM_ENCRYPTION.with(|m| m.borrow_mut().insert(file_hash.clone(), config));
    }
    Ok(file_hash)
}

/// Phase 2: for every entry in `session.pending_files` that has a stored File object,
/// call start_api, fire INITIALIZED callback, and enqueue for upload.
/// Also handles re-provided resumable files (already in session.files, stored in
/// PENDING_WASM_FILES by start_and_enqueue without going through pending_files).
/// Called from `wasm_resume_all` (JS `resume()`) so JS always stores fileHash first.
pub(crate) fn process_pending_files() {
    let pending: Vec<(String, String, String, u64, HashMap<String, String>)> = {
        let sess = session::session();
        sess.pending_files
            .values()
            .map(|p| {
                (
                    p.file_hash.clone(),
                    p.transfer_id.clone(),
                    p.file_name.clone(),
                    p.file_size,
                    p.user_params.clone(),
                )
            })
            .collect()
    };

    for (file_hash, transfer_id, file_name, file_size, user_params) in pending {
        let file_opt = PENDING_WASM_FILES.with(|m| m.borrow().get(&file_hash).cloned());
        let Some(file) = file_opt else { continue };
        let enc_opt = PENDING_WASM_ENCRYPTION.with(|m| m.borrow().get(&file_hash).cloned());
        spawn_local(async move {
            let result = crate::core::upload::start_and_register(
                file_hash.clone(),
                &transfer_id,
                String::new(),
                file_name,
                file_size,
                user_params,
                &WasmApiClient,
            )
            .await;
            match result {
                Ok(result) => {
                    let file_key = result.file_key().to_string();
                    // Check if cancel_all fired while we awaited the start_api network call.
                    // cancel_all clears pending_files; if our hash is gone, the session was reset.
                    let still_active = session::session().pending_files.contains_key(&file_hash);
                    session::session().pending_files.remove(&file_hash);
                    PENDING_WASM_FILES.with(|m| m.borrow_mut().remove(&file_hash));
                    PENDING_WASM_ENCRYPTION.with(|m| m.borrow_mut().remove(&file_hash));
                    if !still_active {
                        // Undo the session.files insertion made by start_and_register.
                        session::session().cancel_file(&file_key);
                        return;
                    }
                    if let Some(config) = enc_opt {
                        session::session().update_encryption_config(&file_key, config);
                    }
                    runtime::notify_file_registered(wasmProgress::progress_manager(), &file_key);
                    if result.should_upload() {
                        enqueue_file(file_key, file);
                    }
                }
                Err(e) => {
                    web_sys::console::error_1(
                        &format!("start_and_register failed for {}: {}", file_hash, e).into(),
                    );
                }
            }
        });
    }

    // Handle re-provided resumable files: stored in PENDING_WASM_FILES by start_and_enqueue
    // but skipped pre_register_file (so they're NOT in pending_files).
    let pending_file_hashes: std::collections::HashSet<String> =
        session::session().pending_files.keys().cloned().collect();
    let resumable: Vec<(String, web_sys::File)> = PENDING_WASM_FILES.with(|m| {
        m.borrow()
            .iter()
            .filter(|(hash, _)| !pending_file_hashes.contains(*hash))
            .map(|(hash, file)| (hash.clone(), file.clone()))
            .collect()
    });
    for (file_hash, file) in resumable {
        let file_key = match session::session().find_by_hash(&file_hash) {
            Some(e) if e.is_resumable() => e.file_key.clone(),
            _ => continue,
        };
        PENDING_WASM_FILES.with(|m| m.borrow_mut().remove(&file_hash));
        // Restore the encryption config if the caller re-provided it.
        let enc_opt = PENDING_WASM_ENCRYPTION.with(|m| m.borrow_mut().remove(&file_hash));
        if let Some(config) = enc_opt {
            session::session().update_encryption_config(&file_key, config);
        }
        runtime::notify_file_registered(wasmProgress::progress_manager(), &file_key);
        enqueue_file(file_key, file);
    }
}

pub(crate) fn enqueue_file(file_key: String, file: web_sys::File) {
    FILE_QUEUE.with(|q| q.borrow_mut().push_back(FileQueueEntry { file_key, file }));
    maybe_start_next();
}

pub(crate) fn is_pause_requested() -> bool {
    PAUSE_REQUESTED.with(|p| *p.borrow())
}

pub(crate) fn resume_queue() {
    PAUSE_REQUESTED.with(|p| *p.borrow_mut() = false);
    maybe_start_next();
}

fn maybe_start_next() {
    if is_pause_requested() {
        return;
    }
    if QUEUE_RUNNING.with(|r| *r.borrow()) {
        return;
    }
    let next = FILE_QUEUE.with(|q| q.borrow_mut().pop_front());
    if let Some(entry) = next {
        QUEUE_RUNNING.with(|r| *r.borrow_mut() = true);
        spawn_local(async move {
            let outcome = upload::run_upload(&entry.file_key, entry.file.clone()).await;
            if matches!(outcome, UploadOutcome::Paused) {
                FILE_QUEUE.with(|q| q.borrow_mut().push_front(entry));
            }
            QUEUE_RUNNING.with(|r| *r.borrow_mut() = false);
            if !is_pause_requested() {
                maybe_start_next();
            }
        });
    }
}
