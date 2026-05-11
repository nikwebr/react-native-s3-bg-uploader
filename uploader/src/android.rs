mod bindings;
mod hash;
pub mod progress;
mod upload;

use std::collections::HashMap;
use std::fs::File;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, Once};
use std::thread;

use crate::core::{session, upload as core_upload};
use crate::core::session::{PendingFileEntry, UploadState};
use crate::core::upload::StartResult;

struct DescriptorForFileKey {
    file_key: String,
    raw_fd: RawFd,
}

static QUEUE: Mutex<std::collections::VecDeque<DescriptorForFileKey>> =
    Mutex::new(std::collections::VecDeque::new());
pub(crate) static QUEUE_SIGNAL: Condvar = Condvar::new();
static WORKER_STARTED: Once = Once::new();
pub(crate) static PAUSE_FLAG: AtomicBool = AtomicBool::new(false);
/// Holds raw fds for files that failed so they can be re-enqueued on resume.
pub(crate) static FAILED_FDS: std::sync::LazyLock<Mutex<HashMap<String, RawFd>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

use crate::native::{BlockingNetwork, NativeApiClient};

/// Phase 1: hash fd and pre-register (before start_api). Returns file hash.
pub(super) fn hash_and_pre_register(
    raw_fd: RawFd,
    file_name: String,
    transfer_id: &str,
    user_params: HashMap<String, String>,
) -> Result<String, String> {
    init_logging();
    let file_hash = hash::hash_fd(raw_fd, transfer_id)?;
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
    let dup_for_size = unsafe { libc::dup(raw_fd) };
    let file_size = if dup_for_size != -1 {
        unsafe { File::from_raw_fd(dup_for_size).metadata().map(|m| m.len()).unwrap_or(0) }
    } else {
        0
    };
    session::session().pre_register_file(
        file_hash.clone(),
        transfer_id.to_string(),
        String::new(),
        file_name,
        file_size,
        user_params,
    );
    Ok(file_hash)
}

/// Phase 2: call start_api and enqueue. `raw_fd` is a fresh fd opened by Kotlin for this phase.
pub(super) fn initialize_and_enqueue(
    raw_fd: RawFd,
    file_hash: &str,
    transfer_id: &str,
) -> Result<String, String> {
    let pending: PendingFileEntry = session::session()
        .pending_files
        .get(file_hash)
        .cloned()
        .ok_or_else(|| format!("no pending entry for hash {}", file_hash))?;

    let dup_for_size = unsafe { libc::dup(raw_fd) };
    let file_size = if dup_for_size != -1 {
        unsafe { File::from_raw_fd(dup_for_size).metadata().map(|m| m.len()).unwrap_or(0) }
    } else {
        pending.file_size
    };

    let api = NativeApiClient { network: AndroidNetwork { client: build_client() } };
    let result = pollster::block_on(core_upload::start_and_register(
        file_hash.to_string(),
        transfer_id,
        String::new(),
        pending.file_name.clone(),
        file_size,
        pending.user_params.clone(),
        &api,
    ))?;
    // Check if cancel_all fired while we were blocked on the network call.
    let still_active = session::session().pending_files.contains_key(file_hash);
    session::session().pending_files.remove(file_hash);
    if !still_active {
        session::session().cancel_file(result.file_key());
        return Err("cancelled".to_string());
    }
    if result.should_upload() {
        let dup = dup_fd(raw_fd).map_err(|e| e.to_string())?;
        enqueue(result.file_key().to_string(), dup);
    }
    Ok(result.file_key().to_string())
}

pub(crate) fn enqueue(file_key: String, raw_fd: RawFd) {
    start_worker_thread();
    let has_progress = session::session()
        .files
        .get(&file_key)
        .map(|e| e.uploaded_bytes > 0)
        .unwrap_or(false);
    let mut q = QUEUE.lock().unwrap();
    let entry = DescriptorForFileKey { file_key, raw_fd };
    // Files that already have progress go to the front so they resume before fresh files.
    if has_progress {
        q.push_front(entry);
    } else {
        q.push_back(entry);
    }
    QUEUE_SIGNAL.notify_one();
}

pub(crate) fn enqueue_front(file_key: String, raw_fd: RawFd) {
    start_worker_thread();
    QUEUE
        .lock()
        .unwrap()
        .push_front(DescriptorForFileKey { file_key, raw_fd });
    QUEUE_SIGNAL.notify_one();
}

#[derive(Clone)]
pub(crate) struct AndroidNetwork {
    pub client: reqwest::blocking::Client,
}

pub(crate) fn build_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .connect_timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
}

pub(crate) fn dup_fd(fd: RawFd) -> std::io::Result<RawFd> {
    let new_fd = unsafe { libc::dup(fd) };
    if new_fd == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(new_fd)
    }
}

fn start_worker_thread() {
    WORKER_STARTED.call_once(|| {
        thread::spawn(|| loop {
            let pending = {
                let mut q = QUEUE.lock().unwrap();
                loop {
                    // Keep paused uploads queued until resume_all() clears the pause flag.
                    if !PAUSE_FLAG.load(std::sync::atomic::Ordering::Relaxed) {
                        if let Some(p) = q.pop_front() {
                            break p;
                        }
                    }
                    q = QUEUE_SIGNAL.wait(q).unwrap();
                }
            };
            upload::run_upload(&pending.file_key, pending.raw_fd);
        });
    });
}

impl BlockingNetwork for AndroidNetwork {
    fn post_json(
        &self,
        url: &str,
        body_json: String,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body_json)
            .send()?;
        response.text().map_err(|e| e.into())
    }
}

fn init_logging() {
    let _ = android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            .with_tag("S3Uploader"),
    );
}
