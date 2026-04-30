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

use crate::native::{BlockingNetwork, NativeApiClient};

struct DescriptorForFileKey {
    file_key: String,
    raw_fd: RawFd,
}

static QUEUE: Mutex<std::collections::VecDeque<DescriptorForFileKey>> =
    Mutex::new(std::collections::VecDeque::new());
pub(crate) static QUEUE_SIGNAL: Condvar = Condvar::new();
static WORKER_STARTED: Once = Once::new();
pub(crate) static PAUSE_FLAG: AtomicBool = AtomicBool::new(false);

pub(super) fn start_and_enqueue(
    raw_fd: RawFd,
    transfer_id: &str,
    user_params: HashMap<String, String>,
) -> Result<String, String> {
    init_logging();
    let file_hash = hash::sha256_fd(raw_fd, transfer_id)?;
    let dup_for_size = unsafe { libc::dup(raw_fd) };
    let file_size = if dup_for_size != -1 {
        unsafe { File::from_raw_fd(dup_for_size).metadata().map(|m| m.len()).unwrap_or(0) }
    } else {
        0
    };
    let api = NativeApiClient { network: AndroidNetwork { client: build_client() } };
    let result = pollster::block_on(crate::core::upload::start_and_register(
        file_hash,
        transfer_id,
        String::new(),
        "file".to_string(),
        file_size,
        user_params,
        &api,
    ))?;
    if result.should_upload() {
        let dup = dup_fd(raw_fd).map_err(|e| e.to_string())?;
        enqueue(result.file_key().to_string(), dup);
    }
    Ok(result.file_key().to_string())
}

pub(crate) fn enqueue(file_key: String, raw_fd: RawFd) {
    start_worker_thread();
    QUEUE
        .lock()
        .unwrap()
        .push_back(DescriptorForFileKey { file_key, raw_fd });
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
