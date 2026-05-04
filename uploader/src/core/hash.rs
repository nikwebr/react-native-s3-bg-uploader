use std::io::{Cursor, Read, Seek, SeekFrom};

const SAMPLE_THRESHOLD: u64 = 128 * 1024;
const SAMPLE_SIZE: usize = 16 * 1024;

pub fn hash_read_seek<R: Read + Seek>(reader: &mut R, transfer_id: &str) -> Result<String, String> {
    let size = reader
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("Failed to seek: {}", e))?;
    reader
        .rewind()
        .map_err(|e| format!("Failed to rewind: {}", e))?;

    let file_samples = if size < SAMPLE_THRESHOLD || size < (4 * SAMPLE_SIZE) as u64 {
        let mut buf = Vec::new();
        reader
            .read_to_end(&mut buf)
            .map_err(|e| format!("Failed to read: {}", e))?;
        buf
    } else {
        let mut first = vec![0u8; SAMPLE_SIZE];
        reader
            .read_exact(&mut first)
            .map_err(|e| format!("Failed to read first sample: {}", e))?;
        reader
            .seek(SeekFrom::Start(size / 2))
            .map_err(|e| format!("Failed to seek middle: {}", e))?;
        let mut middle = vec![0u8; SAMPLE_SIZE];
        reader
            .read_exact(&mut middle)
            .map_err(|e| format!("Failed to read middle sample: {}", e))?;
        reader
            .seek(SeekFrom::End(-(SAMPLE_SIZE as i64)))
            .map_err(|e| format!("Failed to seek end: {}", e))?;
        let mut last = vec![0u8; SAMPLE_SIZE];
        reader
            .read_exact(&mut last)
            .map_err(|e| format!("Failed to read last sample: {}", e))?;
        let mut buf = first;
        buf.extend(middle);
        buf.extend(last);
        buf
    };

    let mut buffer = transfer_id.as_bytes().to_vec();
    buffer.extend(file_samples);

    let hash_result = murmur3::murmur3_x64_128(&mut Cursor::new(buffer), 0)
        .map_err(|e| format!("murmur3 failed: {}", e))?;
    let mut hash_bytes = hash_result.rotate_right(64).swap_bytes().to_le_bytes();
    put_uvarint(&mut hash_bytes, size);
    Ok(format!("{:032x}", u128::from_le_bytes(hash_bytes)))
}

fn put_uvarint(buffer: &mut [u8], x: u64) {
    let mut i = 0;
    let mut mx = x;
    while mx >= 0x80 {
        buffer[i] = mx as u8 | 0x80;
        mx >>= 7;
        i += 1;
    }
    buffer[i] = mx as u8;
}
