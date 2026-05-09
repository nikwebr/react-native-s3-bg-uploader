use crate::core::hash;

pub(super) fn hash_web_file(file: &web_sys::File, transfer_id: &str) -> Result<String, String> {
    let mut reader = wasm_bindgen_file_reader::WebSysFile::new(file.clone());
    hash::hash_read_seek(&mut reader, transfer_id)
}
