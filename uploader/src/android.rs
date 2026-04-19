mod bindings;
pub mod progress;
mod upload;

use std::os::unix::io::RawFd;
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, Once};
use std::thread;

fn init_logging() {
    let _ = android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Debug)
            .with_tag("S3Uploader"),
    );
}

pub(crate) fn dup_fd(fd: RawFd) -> std::io::Result<RawFd> {
    let new_fd = unsafe { libc::dup(fd) };
    if new_fd == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(new_fd)
    }
}

pub(crate) fn build_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .connect_timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
}

struct PendingUpload {
    file_key: String,
    raw_fd: RawFd,
}

static QUEUE: Mutex<std::collections::VecDeque<PendingUpload>> =
    Mutex::new(std::collections::VecDeque::new());
static QUEUE_SIGNAL: Condvar = Condvar::new();
static WORKER_STARTED: Once = Once::new();
pub(crate) static PAUSE_FLAG: AtomicBool = AtomicBool::new(false);

fn start_worker_thread() {
    WORKER_STARTED.call_once(|| {
        thread::spawn(|| loop {
            let pending = {
                let mut q = QUEUE.lock().unwrap();
                loop {
                    if let Some(p) = q.pop_front() {
                        break p;
                    }
                    q = QUEUE_SIGNAL.wait(q).unwrap();
                }
            };
            upload::run_upload(&pending.file_key, pending.raw_fd);
        });
    });
}

pub(crate) fn enqueue(file_key: String, raw_fd: RawFd) {
    start_worker_thread();
    QUEUE
        .lock()
        .unwrap()
        .push_back(PendingUpload { file_key, raw_fd });
    QUEUE_SIGNAL.notify_one();
}
