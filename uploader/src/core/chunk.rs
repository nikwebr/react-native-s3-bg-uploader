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
