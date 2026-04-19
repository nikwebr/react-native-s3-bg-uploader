mod core;

#[cfg(feature = "ios")]
mod ios;

#[cfg(feature = "android")]
mod android;

#[cfg(feature = "wasm")]
mod wasm;

// ---------------------------------------------------------------------------
// C FFI types — defined unconditionally so cbindgen always sees them and can
// emit the correct nullable function-pointer declaration in the header.
// ---------------------------------------------------------------------------

use std::os::raw::c_char;

/// All progress fields in one struct so the C callback takes a single pointer.
/// Swift cannot bridge C function pointers with many parameters, but handles
/// a single `UnsafePointer<ProgressEvent>` fine.
#[repr(C)]
pub struct ProgressEvent {
    // per-file
    pub file_key:                    *const c_char,
    pub transfer_id:                 *const c_char,
    pub total_bytes:                 u64,
    pub uploaded_bytes:              u64,
    pub completed_parts:             u32,
    pub total_parts:                 u32,
    pub percentage:                  f64,
    pub state:                       *const c_char,
    // transfer aggregate
    pub transfer_percentage:         f64,
    pub transfer_total_size:         u64,
    pub transfer_uploaded_size:      u64,
    pub transfer_total_files:        u32,
    pub transfer_completed_files:    u32,
    pub transfer_state:              *const c_char,
    // session aggregate
    pub session_percentage:          f64,
    pub session_total_size:          u64,
    pub session_uploaded_size:       u64,
    pub session_total_transfers:     u32,
    pub session_completed_transfers: u32,
    pub session_total_files:         u32,
    pub session_completed_files:     u32,
    pub session_state:               *const c_char,
}

pub type ProgressCallback = extern "C" fn(*const ProgressEvent);

/// Register (or clear) the iOS progress callback.
/// Defined at crate root so cbindgen can see both the type and the function
/// without needing the `ios` feature to be active during header generation.
/// Pass the null function pointer to clear the callback.
#[cfg(not(feature = "wasm"))]
#[no_mangle]
pub extern "C" fn set_progress_callback(callback: ProgressCallback) {
    #[cfg(feature = "ios")]
    {
        use ios::progress::PROGRESS_CALLBACK;
        // SAFETY: ProgressCallback is extern "C" fn — transmuting a potentially-null
        // function pointer to Option<fn> is valid because Rust guarantees the
        // null-pointer optimisation for non-null fn pointers.
        let opt: Option<ProgressCallback> = unsafe { std::mem::transmute(callback) };
        *PROGRESS_CALLBACK.lock().unwrap() = opt;
    }
}

/*
use std::ffi::CStr;
use std::fs::File;
use std::io::{Read, BufReader};
use std::os::raw::{c_char, c_ulonglong};
use std::ptr;

/// Liest eine Datei chunkweise und berechnet ihre Größe.
///
/// # Safety
/// `path` muss ein gültiger C-String sein.
#[no_mangle]
pub extern "C" fn calculate_file_size(path: *const c_char) -> c_ulonglong {
    if path.is_null() {
        return 0;
    }

    let c_str = unsafe { CStr::from_ptr(path) };

    let file_path = match c_str.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };

    let file = match File::open(file_path) {
        Ok(f) => f,
        Err(_) => return 0,
    };

    let mut reader = BufReader::new(file);
    let mut buffer = [0u8; 8 * 1024]; // 8 KB Chunk
    let mut total_size: u64 = 0;

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break, // EOF
            Ok(n) => total_size += n as u64,
            Err(_) => return 0,
        }
    }

    total_size
}



use wasm_bindgen::prelude::*;
use std::io::{Read, Seek, SeekFrom};
use wasm_bindgen_file_reader::WebSysFile;
use web_sys::console;

/// Reads one byte from the file at a given offset. Returns the read byte or 0 if the file is empty
/// See also https://github.com/Badel2/wasm-bindgen-file-reader-test
#[wasm_bindgen]
pub fn read_at_offset_sync(file: web_sys::File, offset: u64) -> u8 {
    let log_msg = format!(
        "Rust function read_at_offset_sync sees file \"{}\". Size of file in bytes: {}",
        file.name(),
        file.size()
    );
    log_to_browser(log_msg);

    if file.size() == 0.0 {
        log_to_browser("Can't get first byte of an empty file".to_string());
        return 0;
    }
    {
        let mut wf = WebSysFile::new(file);

        // Now we can seek as if this was a real file
        wf.seek(SeekFrom::Start(offset))
            .expect("failed to seek to offset");

        // Use a 1-byte buffer because we only want to read one byte
        let mut buf = [0];

        // The Read API works as with real files
        wf.read_exact(&mut buf).expect("failed to read bytes");

        log_to_browser("success!".to_string());

        buf[0]
    }
}

#[wasm_bindgen]
pub fn size(file: web_sys::File, offset: u64) -> i64 {
    let log_msg = format!(
        "Rust function size sees file \"{}\". Size of file in bytes: {}",
        file.name(),
        file.size()
    );
    log_to_browser(log_msg);

    if file.size() == 0.0 {
        log_to_browser("Can't determine size of an empty file".to_string());
        return 0;
    }

    let mut wf = WebSysFile::new(file);

    // Seek to the specified offset
    wf.seek(SeekFrom::Start(offset))
        .expect("failed to seek to offset");

    let mut total_bytes_read = 0i64;
    let chunk_size = 8192; // 8KB chunks
    let mut buf = vec![0; chunk_size];

    // Read file in chunks from the offset position
    loop {
        match wf.read(&mut buf) {
            Ok(0) => {
                // End of file reached
                log_to_browser(format!("Finished reading. Total bytes from offset: {}", total_bytes_read));
                break;
            }
            Ok(n) => {
                total_bytes_read += n as i64;
            }
            Err(e) => {
                log_to_browser(format!("Error reading file: {:?}", e));
                break;
            }
        }}

    total_bytes_read
}

/// Logs a string to the browser's console
fn log_to_browser(log_msg: String) {
    console::log_1(&log_msg.into());
}

 */



#[cfg(test)]
mod tests {

    #[test]
    fn it_works() {
        //let result = fibonacci(0);
       // assert_eq!(result, 0);
    }
}