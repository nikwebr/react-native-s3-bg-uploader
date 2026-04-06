use std::io::{Cursor, Read, Seek, SeekFrom};
use std::sync::{Mutex, OnceLock};
use crate::core::progress::{ProgressManager, ProgressNotifier, UploadProgress};

static PROGRESS_MANAGER: OnceLock<ProgressManager<AndroidProgressNotifier>> = OnceLock::new();
static PROGRESS_CALLBACK: Mutex<Option<ProgressCallback>> = Mutex::new(None);

// JNI callback: fn(total_bytes, uploaded_bytes, completed_parts, total_parts, percentage, state_str)
pub type ProgressCallback = Box<dyn Fn(u64, u64, u32, u32, f64, &str) + Send + Sync>;

pub struct AndroidProgressNotifier;

impl ProgressNotifier for AndroidProgressNotifier {
    fn notify(&self, progress: &UploadProgress) {
        notify_progress(progress);
    }
}

pub fn progress_manager() -> &'static ProgressManager<AndroidProgressNotifier> {
    PROGRESS_MANAGER.get_or_init(|| ProgressManager::new(AndroidProgressNotifier))
}

pub fn update_progress<F>(update_fn: F)
where
    F: FnOnce(&ProgressManager<AndroidProgressNotifier>),
{
    let manager = progress_manager();
    update_fn(manager);
}

pub fn set_progress_callback(callback: Option<ProgressCallback>) {
    let mut cb = PROGRESS_CALLBACK.lock().unwrap();
    *cb = callback;
}

fn notify_progress(progress: &UploadProgress) {
    let cb = PROGRESS_CALLBACK.lock().unwrap();
    if let Some(ref callback) = *cb {
        callback(
            progress.total_bytes,
            progress.uploaded_bytes(),
            progress.completed_parts,
            progress.total_parts,
            progress.percentage(),
            progress.status.as_str(),
        );
    }
}

pub struct ProgressReader {
    inner: Cursor<Vec<u8>>,
    part_number: u32,
    bytes_read: u64,
}

impl ProgressReader {
    pub fn new(data: Vec<u8>, part_number: u32) -> Self {
        Self {
            inner: Cursor::new(data),
            part_number,
            bytes_read: 0,
        }
    }
}

impl Read for ProgressReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.bytes_read += n as u64;
            update_progress(|m| m.update_in_flight(self.part_number, self.bytes_read));
        }
        Ok(n)
    }
}

impl Seek for ProgressReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new_pos = self.inner.seek(pos)?;
        // Reset bei Seek (für Retry-Szenarien)
        self.bytes_read = new_pos;
        Ok(new_pos)
    }
}
