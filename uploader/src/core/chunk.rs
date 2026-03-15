use crate::core::api::UploadUrlsResponse;
use std::io::{Read, Seek, SeekFrom};

/// Informationen für einen einzelnen Chunk-Upload
#[derive(Debug, Clone)]
pub struct ChunkInfo {
    pub part_number: u32,
    pub start_pos: u64,
    pub chunk_size: u64,
    pub url: String,
}

impl ChunkInfo {
    fn new(part_number: u32, chunk_size: u64, file_size: u64, url: String) -> Self {
        let (start_pos, chunk_size) = calculate_chunk_bounds(part_number, chunk_size, file_size);
        Self {
            part_number,
            start_pos,
            chunk_size,
            url,
        }
    }

    pub fn read<R>(&self, reader: &mut R) -> std::io::Result<Vec<u8>> where R: Read + Seek
    {
        reader.seek(SeekFrom::Start(self.start_pos))?;

        let mut buffer = vec![0u8; self.chunk_size as usize];
        let mut total_read = 0;

        while total_read < (self.chunk_size as usize) {
            match reader.read(&mut buffer[total_read..]) {
                Ok(0) => break, // EOF
                Ok(n) => total_read += n,
                Err(e) => return Err(e),
            }
        }

        buffer.truncate(total_read);
        Ok(buffer)
    }
}

/// Generiert alle ChunkInfo-Objekte für einen Upload
pub fn generate_chunk_infos(
    file_size: u64,
    upload_info: &UploadUrlsResponse,
) -> Result<Vec<ChunkInfo>, String> {
    let total_parts = upload_info.chunk_count() as usize;
    let mut chunks = Vec::with_capacity(total_parts);

    for part_number in 1..=total_parts {
        let url = upload_info
            .urls
            .get(&part_number.to_string())
            .ok_or_else(|| format!("No URL for part {}", part_number))?
            .clone();

        chunks.push(ChunkInfo::new(part_number as u32, upload_info.part_size, file_size, url));
    }

    Ok(chunks)
}

fn calculate_chunk_bounds(part_number: u32, chunk_size: u64, file_size: u64) -> (u64, u64) {
    let start_pos = ((part_number - 1) as u64) * chunk_size;
    let remaining = file_size - start_pos;
    let chunk_size = std::cmp::min(remaining, chunk_size);
    (start_pos, chunk_size)
}
