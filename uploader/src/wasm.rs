pub mod api;
mod bindings;
pub mod progress;
mod upload;

use std::cell::RefCell;
use std::collections::VecDeque;

use wasm_bindgen_futures::spawn_local;

use crate::core::runtime;
use crate::core::session::{self, GlobalUploaderState, UploadState};
use crate::wasm::progress as wasmProgress;

struct FileQueueEntry {
    file_key: String,
    file: web_sys::File,
}

thread_local! {
    pub(crate) static FILE_QUEUE: RefCell<VecDeque<FileQueueEntry>> = RefCell::new(VecDeque::new());
    pub(crate) static QUEUE_RUNNING: RefCell<bool> = RefCell::new(false);
    pub(crate) static PAUSE_REQUESTED: RefCell<bool> = RefCell::new(false);
}

pub(crate) fn enqueue_file(file_key: String, file: web_sys::File) {
    FILE_QUEUE.with(|q| q.borrow_mut().push_back(FileQueueEntry { file_key, file }));
    maybe_start_next();
}

pub(crate) fn is_pause_requested() -> bool {
    PAUSE_REQUESTED.with(|p| *p.borrow())
}

#[derive(Debug)]
pub(crate) enum UploadOutcome {
    Completed,
    Paused,
    Failed(String),
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
            session::session().mark_file_state(&entry.file_key, UploadState::Running);
            let outcome = upload::run_upload(&entry.file_key, entry.file).await;
            match outcome {
                UploadOutcome::Completed => {
                    session::session().mark_file_state(&entry.file_key, UploadState::Completed);
                    runtime::set_status(
                        wasmProgress::progress_manager(),
                        &entry.file_key,
                        UploadState::Completed,
                    );
                    let all_done =
                        session::session().global_state == GlobalUploaderState::Completed;
                    if all_done {
                        session::clear_session();
                        wasmProgress::progress_manager().clear();
                    } else {
                        session::persist_session();
                    }
                }
                UploadOutcome::Paused => {
                    session::session().mark_file_state(&entry.file_key, UploadState::Paused);
                    session::persist_session();
                    PAUSE_REQUESTED.with(|p| *p.borrow_mut() = false);
                }
                UploadOutcome::Failed(e) => {
                    web_sys::console::error_1(
                        &format!("Upload failed for {}: {}", entry.file_key, e).into(),
                    );
                    session::session().mark_file_state(&entry.file_key, UploadState::Failed);
                    session::persist_session();
                    runtime::set_status(
                        wasmProgress::progress_manager(),
                        &entry.file_key,
                        UploadState::Failed,
                    );
                }
            }

            QUEUE_RUNNING.with(|r| *r.borrow_mut() = false);
            maybe_start_next();
        });
    }
}
