mod core;

#[cfg(not(target_arch = "wasm32"))]
mod native;

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
    pub file_key: *const c_char,
    pub transfer_id: *const c_char,
    pub total_bytes: u64,
    pub uploaded_bytes: u64,
    pub completed_parts: u32,
    pub total_parts: u32,
    pub percentage: f64,
    pub state: *const c_char,
    // transfer aggregate
    pub transfer_percentage: f64,
    pub transfer_total_size: u64,
    pub transfer_uploaded_size: u64,
    pub transfer_total_files: u32,
    pub transfer_completed_files: u32,
    pub transfer_state: *const c_char,
    // session aggregate
    pub session_percentage: f64,
    pub session_total_size: u64,
    pub session_uploaded_size: u64,
    pub session_total_transfers: u32,
    pub session_completed_transfers: u32,
    pub session_total_files: u32,
    pub session_completed_files: u32,
    pub session_state: *const c_char,
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

#[cfg(test)]
mod tests {

    #[test]
    fn it_works() {
        //let result = fibonacci(0);
        // assert_eq!(result, 0);
    }
}
