mod bindings;
pub mod progress;
mod upload;

use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, Once};
use std::thread;

pub(crate) static INIT: Once = Once::new();

pub(crate) fn init_nyquest() {
    INIT.call_once(|| {
        nyquest_backend_nsurlsession::register();
    });
}

pub(crate) struct UploadQueue {
    pub pending: std::collections::VecDeque<String>,
}

pub(crate) static QUEUE: Mutex<UploadQueue> = Mutex::new(UploadQueue {
    pending: std::collections::VecDeque::new(),
});
pub(crate) static QUEUE_SIGNAL: Condvar = Condvar::new();
static WORKER_STARTED: Once = Once::new();
pub(crate) static PAUSE_FLAG: AtomicBool = AtomicBool::new(false);

fn start_worker_thread() {
    WORKER_STARTED.call_once(|| {
        thread::spawn(|| loop {
            let file_key = {
                let mut q = QUEUE.lock().unwrap();
                loop {
                    if let Some(k) = q.pending.pop_front() {
                        break k;
                    }
                    q = QUEUE_SIGNAL.wait(q).unwrap();
                }
            };
            upload::run_upload(&file_key);
        });
    });
}

pub(crate) fn enqueue_key(file_key: String) {
    start_worker_thread();
    let mut q = QUEUE.lock().unwrap();
    q.pending.push_back(file_key);
    QUEUE_SIGNAL.notify_one();
}
