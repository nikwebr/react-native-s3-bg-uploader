use sha2::{Digest, Sha256};
use std::io::Read;

const CHUNK_SIZE: usize = 100 * 1024 * 1024; // 4 MB read buffer

/// Compute SHA-256 of (transfer_id || file content) from a file path.
/// Using transfer_id as a prefix means the same file in different transfers
/// produces different hashes and is treated as an independent upload entry.
pub fn sha256_file(path: &str, transfer_id: &str) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(transfer_id.as_bytes());

    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open file for hashing: {}", e))?;

    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("Failed to read file for hashing: {}", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute SHA-256 of (transfer_id || file content) from a raw file descriptor.
/// Duplicates the fd before reading so it does not consume ownership.
#[cfg(feature = "android")]
pub fn sha256_fd(raw_fd: std::os::unix::io::RawFd, transfer_id: &str) -> Result<String, String> {
    use std::fs::File;
    use std::os::unix::io::FromRawFd;

    let new_fd = unsafe { libc::dup(raw_fd) };
    if new_fd == -1 {
        return Err("dup() failed for hashing fd".to_string());
    }

    let mut hasher = Sha256::new();
    hasher.update(transfer_id.as_bytes());

    let mut file = unsafe { File::from_raw_fd(new_fd) };
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("Failed to read fd for hashing: {}", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute SHA-256 of (transfer_id || file content) for a web_sys::File.
/// Reads the entire file via wasm-bindgen-file-reader.
#[cfg(feature = "wasm")]
pub async fn sha256_web_file(
    file: &web_sys::File,
    transfer_id: &str,
) -> Result<String, String> {
    use wasm_bindgen_file_reader::WebSysFile;
    use std::io::Read;

    let mut hasher = Sha256::new();
    hasher.update(transfer_id.as_bytes());

    let mut wf = WebSysFile::new(file.clone());
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = wf
            .read(&mut buf)
            .map_err(|e| format!("Failed to read web file for hashing: {}", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}
