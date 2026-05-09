use std::fs::File;
use std::os::unix::io::{FromRawFd, RawFd};

use crate::core::hash;

pub(super) fn hash_fd(raw_fd: RawFd, transfer_id: &str) -> Result<String, String> {
    let new_fd = unsafe { libc::dup(raw_fd) };
    if new_fd == -1 {
        return Err("dup() failed for hashing fd".to_string());
    }
    let mut file = unsafe { File::from_raw_fd(new_fd) };
    hash::hash_read_seek(&mut file, transfer_id)
}
