#!/usr/bin/env node
// Reads the wasm-bindgen output from target/wasm/ and generates
// example-app/web-encryptor.ts with the JS glue code and WASM binary
// embedded as strings. The factory function is self-contained and can be
// serialized via .toString() and eval'd inside the uploader's worker.
//
// Run after building the WASM module:
//   cd custom-encryptor-rust
//   cargo build --features wasm --target wasm32-unknown-unknown --release
//   wasm-bindgen --out-dir target/wasm --target no-modules \
//     target/wasm32-unknown-unknown/release/custom_encryptor.wasm
//   node scripts/embed-wasm.js

const fs = require('fs')
const path = require('path')

const wasmDir = path.join(__dirname, '../target/wasm')
const outFile = path.join(__dirname, '../../example-app/web-encryptor.ts')

const jsCodeRaw = fs.readFileSync(path.join(wasmDir, 'custom_encryptor.js'), 'utf-8')
const wasmBytes = fs.readFileSync(path.join(wasmDir, 'custom_encryptor_bg.wasm'))
const wasmBase64 = wasmBytes.toString('base64')

const jsCode = jsCodeRaw

const content = `/// <reference lib="dom" />
/**
 * Browser implementation of the custom AES-256-GCM encryptor (WASM build).
 *
 * The Rust code in custom-encryptor-rust/ is compiled to WebAssembly and
 * embedded here. The same encryption logic runs on iOS, Android, and Web.
 *
 * Wire format — identical across all platforms:
 *
 *   per part : AES-256-GCM(part plaintext)
 *              nonce = baseNonce[0..12] with the little-endian part number XORed
 *              into bytes 0..3 (so a resumed part re-encrypts identically)
 *   last part: the same, with the 28-byte descriptor appended:
 *              AES-256-GCM(n_data_parts (u32 LE) ‖ effective_part_size (u64 LE)),
 *              encrypted with part_number = n_data_parts + 1
 *
 * IMPORTANT — this factory is serialized with .toString() and evaluated inside
 * the uploader's worker, so all data must be inlined (no external references).
 */

export const WEB_ENCRYPTOR_OVERHEAD = 16
export const WEB_ENCRYPTOR_HAS_FOOTER = true
export const WEB_ENCRYPTOR_FOOTER_LEN = 28

export const webEncryptorFactory = (keyBytes: Uint8Array, nonceBytes: Uint8Array) => {
  // Embedded wasm-bindgen JS glue (inlined so .toString() serialization works).
  const _js = ${JSON.stringify(jsCode)}

  // Embedded WASM binary as base64 (inlined for the same reason).
  const _wasmB64 = ${JSON.stringify(wasmBase64)}

  // The no-modules glue exposes its exports through wasm_bindgen.
  const wasmModule = new Function(_js + '; return wasm_bindgen;')()

  // Decode the embedded WASM binary
  const wasmBytes = Uint8Array.from(atob(_wasmB64), function(c) { return c.charCodeAt(0); })

  // Initialize the WASM module synchronously and get the WasmEncryptor class.
  wasmModule.initSync({ module: wasmBytes })
  const { WasmEncryptor } = wasmModule

  // Create the encryptor instance
  const keyHex = Array.from(keyBytes, function(byte) { return byte.toString(16).padStart(2, '0'); }).join('')
  const nonceHex = Array.from(nonceBytes, function(byte) { return byte.toString(16).padStart(2, '0'); }).join('')
  const enc = new WasmEncryptor(keyHex, nonceHex)

  return {
    /** AES-GCM appends a 16-byte tag to every part. */
    overhead: enc.overhead,
    /** The format ends with a descriptor that rides inside the last data part. */
    hasFooter: enc.has_footer,
    /** Descriptor: 12 bytes of plaintext + 16-byte tag. */
    footerLen: enc.footer_len,

    /** Called once per data part; partNumber is 1-based and stable across resumes. */
    encryptChunk: (input: Uint8Array, partNumber: number) => enc.encrypt_chunk(input, partNumber),

    /** Called for the final data part only: what a reader needs to decode the object. */
    computeFooter: (nDataParts: number, effectivePartSize: number) => enc.compute_footer(nDataParts, BigInt(effectivePartSize)),
  }
}
`

fs.mkdirSync(path.dirname(outFile), { recursive: true })
fs.writeFileSync(outFile, content)
console.log('✓ Web encryptor generated →', path.relative(process.cwd(), outFile))
