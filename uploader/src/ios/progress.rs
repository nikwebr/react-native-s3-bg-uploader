use std::ffi::CString;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::os::raw::c_char;
use std::sync::{Mutex, OnceLock};
use crate::core::progress::{ProgressManager, ProgressNotifier, UploadProgress};
use crate::ios::iosProgress;

static PROGRESS_MANAGER: OnceLock<ProgressManager<IosProgressNotifier>> = OnceLock::new();
static PROGRESS_CALLBACK: Mutex<Option<ProgressCallback>> = Mutex::new(None);

#[no_mangle]
pub extern "C" fn get_upload_progress() -> f64 {
    iosProgress::progress_manager().percentage()
}

#[no_mangle]
pub extern "C" fn get_upload_progress_json() -> *const c_char {
    if let Some(json) = iosProgress::progress_manager().to_json() {
        if let Ok(json_str) = serde_json::to_string(&json) {
            if let Ok(c_string) = CString::new(json_str) {
                return c_string.into_raw();
            }
        }
    }

    std::ptr::null()
}

#[no_mangle]
pub extern "C" fn set_progress_callback(callback: Option<ProgressCallback>) {
    let mut cb = PROGRESS_CALLBACK.lock().unwrap();
    *cb = callback;
}

pub struct IosProgressNotifier;
pub type ProgressCallback = extern "C" fn(u64, u64, u32, u32, f64);


impl ProgressNotifier for IosProgressNotifier {
    fn notify(&self, progress: &UploadProgress) {
        notify_progress(progress);
    }
}

pub fn progress_manager() -> &'static ProgressManager<IosProgressNotifier> {
    PROGRESS_MANAGER.get_or_init(|| ProgressManager::new(IosProgressNotifier))
}

pub fn update_progress<F>(update_fn: F)
where
    F: FnOnce(&ProgressManager<IosProgressNotifier>),
{
    let manager = progress_manager();
    update_fn(manager);
}

pub struct ProgressReader {
    inner: Cursor<Vec<u8>>,
    part_number: u32,
    bytes_read: u64,
    total_size: u64,
}

impl ProgressReader {
    pub fn new(data: Vec<u8>, part_number: u32) -> Self {
        let total_size = data.len() as u64;
        Self {
            inner: Cursor::new(data),
            part_number,
            bytes_read: 0,
            total_size,
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
        // Reset bytes_read bei Seek (für Retry-Szenarien)
        self.bytes_read = new_pos;
        Ok(new_pos)
    }
}

fn notify_progress(progress: &UploadProgress) {
    let callback = PROGRESS_CALLBACK.lock().unwrap();
    if let Some(cb) = *callback {
        cb(
            progress.total_bytes,
            progress.uploaded_bytes(),
            progress.completed_parts,
            progress.total_parts,
            progress.percentage(),
        );
    }
}