use js_sys::{ArrayBuffer, Uint8Array};
use sha2::{Digest, Sha256};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

const CHUNK_SIZE: f64 = 8.0 * 1024.0 * 1024.0; // 8 MB per slice

pub async fn sha256_web_file(file: &web_sys::File, transfer_id: &str) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(transfer_id.as_bytes());

    let file_size = file.size();
    let mut offset: f64 = 0.0;

    while offset < file_size {
        let end = (offset + CHUNK_SIZE).min(file_size);
        let blob = file
            .slice_with_f64_and_f64(offset, end)
            .map_err(|e| format!("slice() failed: {:?}", e))?;

        let buffer: ArrayBuffer = JsFuture::from(blob.array_buffer())
            .await
            .map_err(|e| format!("array_buffer() failed: {:?}", e))?
            .dyn_into()
            .map_err(|_| "array_buffer() did not return ArrayBuffer".to_string())?;

        let view = Uint8Array::new(&buffer);
        let mut bytes = vec![0u8; view.length() as usize];
        view.copy_to(&mut bytes);
        hasher.update(&bytes);

        offset = end;
    }

    let hash = hasher.finalize();
    Ok(hash.iter().map(|b| format!("{:02x}", b)).collect())
}
