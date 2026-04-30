pub mod api;
mod bindings;
pub mod hash;
pub mod progress;
pub mod store;
mod upload;
pub mod upload_engine;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use wasm_bindgen_futures::spawn_local;

use crate::core::upload::StartResult;
use crate::core::upload_orchestrator::UploadOutcome;
use crate::wasm::api::WasmApiClient;

struct FileQueueEntry {
    file_key: String,
    file: web_sys::File,
}

thread_local! {
    pub(crate) static FILE_QUEUE: RefCell<VecDeque<FileQueueEntry>> = RefCell::new(VecDeque::new());
    pub(crate) static QUEUE_RUNNING: RefCell<bool> = RefCell::new(false);
    pub(crate) static PAUSE_REQUESTED: RefCell<bool> = RefCell::new(false);
}

pub(super) async fn start_and_enqueue(
    file: web_sys::File,
    transfer_id: String,
    user_params: HashMap<String, String>,
) -> Result<String, String> {
    let file_hash = hash::sha256_web_file(&file, &transfer_id).await?;
    let result = crate::core::upload::start_and_register(
        file_hash,
        &transfer_id,
        String::new(),
        file.name(),
        file.size() as u64,
        user_params,
        &WasmApiClient,
    )
    .await?;
    let file_key = result.file_key().to_string();
    if result.should_upload() {
        enqueue_file(file_key.clone(), file);
    }
    Ok(file_key)
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
