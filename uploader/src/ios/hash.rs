use crate::core::hash;

pub(super) fn hash_file(path: &str, transfer_id: &str) -> Result<String, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open file for hashing: {}", e))?;
    hash::hash_read_seek(&mut file, transfer_id)
}
