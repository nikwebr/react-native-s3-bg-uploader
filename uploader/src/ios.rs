mod bindings;
mod hash;
pub mod progress;
mod upload;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, Once};
use std::thread;

use crate::core::{session, upload as core_upload};
use crate::core::session::UploadState;
use crate::core::upload::StartResult;
use crate::native::{BlockingNetwork, NativeApiClient};

pub(crate) static INIT: Once = Once::new();

pub(crate) struct UploadQueue {
    pub pending: std::collections::VecDeque<String>,
    pub pending_keys: HashSet<String>,
}

pub(crate) static QUEUE: std::sync::LazyLock<Mutex<UploadQueue>> =
    std::sync::LazyLock::new(|| {
        Mutex::new(UploadQueue {
            pending: std::collections::VecDeque::new(),
            pending_keys: HashSet::new(),
        })
    });
pub(crate) static QUEUE_SIGNAL: Condvar = Condvar::new();
static WORKER_STARTED: Once = Once::new();
pub(crate) static PAUSE_FLAG: AtomicBool = AtomicBool::new(false);

/// Phase 1: hash the file and pre-register it in the session (before start_api).
/// Returns the file hash on success.
pub(super) fn hash_and_pre_register(
    file_path: &str,
    transfer_id: &str,
    user_params: HashMap<String, String>,
) -> Result<String, String> {
    let file_hash = hash::hash_file(file_path, transfer_id)?;
    {
        let sess = session::session();
        if let Some(entry) = sess.find_by_hash(&file_hash) {
            if entry.state == UploadState::Completed {
                return Ok(file_hash);
            }
            if !entry.is_resumable() {
                return Err(format!("DUPLICATE_FILE: file with hash {} is already active in this session", file_hash));
            }
        } else if sess.pending_files.contains_key(&file_hash) {
            return Err(format!("DUPLICATE_FILE: file with hash {} is already pending in this session", file_hash));
        }
    }
    let file_name = std::path::Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let file_size = std::fs::metadata(file_path)
        .map_err(|e| e.to_string())?
        .len();
    session::session().pre_register_file(
        file_hash.clone(),
        transfer_id.to_string(),
        file_path.to_string(),
        file_name,
        file_size,
        user_params,
    );
    Ok(file_hash)
}

/// Phase 2: call start_api for a pre-registered file, then enqueue it for upload.
/// Returns the file key on success.
pub(super) fn initialize_and_enqueue(
    file_hash: &str,
    transfer_id: &str,
) -> Result<String, String> {
    init_nyquest();
    let pending = session::session()
        .pending_files
        .get(file_hash)
        .cloned()
        .ok_or_else(|| format!("no pending entry for hash {}", file_hash))?;

    let client = nyquest::ClientBuilder::default()
        .request_timeout(std::time::Duration::from_secs(30))
        .build_blocking()
        .map_err(|e| format!("Failed to create client: {:?}", e))?;
    let api = NativeApiClient { network: IosNetwork { client } };
    let result = pollster::block_on(core_upload::start_and_register(
        file_hash.to_string(),
        transfer_id,
        pending.file_path.clone(),
        pending.file_name.clone(),
        pending.file_size,
        pending.user_params.clone(),
        &api,
    ))?;
    // Check if cancel_all fired while we were blocked on the network call.
    // cancel_all clears pending_files; if our hash is gone, the session was reset.
    let still_active = session::session().pending_files.contains_key(file_hash);
    session::session().pending_files.remove(file_hash);
    if !still_active {
        // Undo the session.files insertion made by start_and_register.
        session::session().cancel_file(result.file_key());
        return Err("cancelled".to_string());
    }
    if result.should_upload() {
        if let StartResult::Resumed(ref file_key) = result {
            session::session().update_file_path(file_key, pending.file_path.clone());
        }
        enqueue_key(result.file_key().to_string());
    }
    Ok(result.file_key().to_string())
}

pub(crate) fn enqueue_key(file_key: String) {
    start_worker_thread();
    let has_progress = session::session()
        .files
        .get(&file_key)
        .map(|e| e.uploaded_bytes > 0)
        .unwrap_or(false);
    let mut q = QUEUE.lock().unwrap();
    if !q.pending_keys.insert(file_key.clone()) {
        return;
    }
    // Files that already have progress go to the front so they resume before fresh files.
    if has_progress {
        q.pending.push_front(file_key);
    } else {
        q.pending.push_back(file_key);
    }
    QUEUE_SIGNAL.notify_one();
}

#[derive(Clone)]
pub(crate) struct IosNetwork {
    pub client: nyquest::BlockingClient,
}

pub(crate) fn init_nyquest() {
    INIT.call_once(|| {
        nyquest_backend_nsurlsession::register();
    });
}

fn start_worker_thread() {
    WORKER_STARTED.call_once(|| {
        thread::spawn(|| loop {
            let file_key = {
                let mut q = QUEUE.lock().unwrap();
                loop {
                    // Wait while paused or queue is empty.
                    if !PAUSE_FLAG.load(std::sync::atomic::Ordering::Relaxed) {
                        if let Some(k) = q.pending.pop_front() {
                            q.pending_keys.remove(&k);
                            break k;
                        }
                    }
                    q = QUEUE_SIGNAL.wait(q).unwrap();
                }
            };
            upload::run_upload(&file_key);
        });
    });
}

impl BlockingNetwork for IosNetwork {
    fn post_json(
        &self,
        url: &str,
        body_json: String,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let body = nyquest::Body::bytes(body_json.into_bytes(), "application/json");
        let request = nyquest::Request::post(url.to_string()).with_body(body);
        let response = self.client.request(request)?;
        response.text().map_err(|e| e.into())
    }
}
