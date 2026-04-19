use std::ffi::CString;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::sync::{Mutex, OnceLock};

use crate::core::progress::{ProgressManager, ProgressNotifier};
use crate::core::runtime;
use crate::core::session::{AggregateProgress, FileProgress};
use crate::{ProgressCallback, ProgressEvent};

pub static PROGRESS_CALLBACK: Mutex<Option<ProgressCallback>> = Mutex::new(None);
static PROGRESS_MANAGER: OnceLock<ProgressManager<IosProgressNotifier>> = OnceLock::new();

pub struct IosProgressNotifier;

impl ProgressNotifier for IosProgressNotifier {
    fn notify(
        &self,
        fp: &FileProgress,
        session_agg: &AggregateProgress,
        transfer_agg: &AggregateProgress,
    ) {
        let cb = PROGRESS_CALLBACK.lock().unwrap();
        if let Some(callback) = *cb {
            let file_key = CString::new(fp.file_key.as_str()).unwrap_or_default();
            let transfer_id = CString::new(fp.transfer_id.as_str()).unwrap_or_default();
            let state = CString::new(fp.state.as_str()).unwrap_or_default();
            let t_state = CString::new(transfer_agg.state.as_str()).unwrap_or_default();
            let s_state = CString::new(session_agg.state.as_str()).unwrap_or_default();

            let event = ProgressEvent {
                file_key: file_key.as_ptr(),
                transfer_id: transfer_id.as_ptr(),
                total_bytes: fp.total_bytes,
                uploaded_bytes: fp.uploaded_bytes,
                completed_parts: fp.completed_parts,
                total_parts: fp.total_parts,
                percentage: fp.percentage,
                state: state.as_ptr(),
                transfer_percentage: transfer_agg.percentage,
                transfer_total_size: transfer_agg.total_size,
                transfer_uploaded_size: transfer_agg.uploaded_size,
                transfer_total_files: transfer_agg.total_files,
                transfer_completed_files: transfer_agg.completed_files,
                transfer_state: t_state.as_ptr(),
                session_percentage: session_agg.percentage,
                session_total_size: session_agg.total_size,
                session_uploaded_size: session_agg.uploaded_size,
                session_total_transfers: session_agg.total_transfers.unwrap_or(0),
                session_completed_transfers: session_agg.completed_transfers.unwrap_or(0),
                session_total_files: session_agg.total_files,
                session_completed_files: session_agg.completed_files,
                session_state: s_state.as_ptr(),
            };
            callback(&event);
        }
    }
}

pub fn progress_manager() -> &'static ProgressManager<IosProgressNotifier> {
    PROGRESS_MANAGER.get_or_init(|| ProgressManager::new(IosProgressNotifier))
}

// ---------------------------------------------------------------------------
// ProgressReader — in-flight progress tracking during chunk upload
// ---------------------------------------------------------------------------

const NOTIFY_EVERY_BYTES: u64 = 256 * 1024; // 256 KB

pub struct ProgressReader {
    inner: Cursor<Vec<u8>>,
    file_key: String,
    part_number: u32,
    bytes_read: u64,
    last_notified: u64,
}

impl ProgressReader {
    pub fn new(data: Vec<u8>, file_key: String, part_number: u32) -> Self {
        Self {
            inner: Cursor::new(data),
            file_key,
            part_number,
            bytes_read: 0,
            last_notified: 0,
        }
    }
}

impl Read for ProgressReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if crate::ios::PAUSE_FLAG.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "paused",
            ));
        }
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.bytes_read += n as u64;
            if self.bytes_read - self.last_notified >= NOTIFY_EVERY_BYTES {
                self.last_notified = self.bytes_read;
                runtime::update_in_flight(
                    progress_manager(),
                    &self.file_key,
                    self.part_number,
                    self.bytes_read,
                );
            }
        }
        Ok(n)
    }
}

impl Seek for ProgressReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos = self.inner.seek(pos)?;
        self.bytes_read = new_pos;
        self.last_notified = new_pos;
        Ok(new_pos)
    }
}
