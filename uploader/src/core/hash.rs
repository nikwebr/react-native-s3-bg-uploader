use xxhash_rust::xxh3::Xxh3;
use std::io::Read;

const BUF_SIZE: usize = 64 * 1024 * 1024;

pub fn hash_reader<R: Read>(reader: &mut R, transfer_id: &str) -> Result<String, String> {
    let mut hasher = Xxh3::new();
    hasher.update(transfer_id.as_bytes());
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("Failed to read for hashing: {}", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:016x}", hasher.digest()))
}
